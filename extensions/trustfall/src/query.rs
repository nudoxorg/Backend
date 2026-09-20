//! Lazy async Trustfall execution over one immutable product view.

use backend_library::{Fragment, PackageKey, Row, RowId, ViewRoot, encode_id};
use futures_util::stream;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock};
use thiserror::Error;
use trustfall::provider::async_helpers::{
    resolve_coercion_with, resolve_neighbors_with, resolve_property_with,
};
use trustfall::provider::{
    AsVertex, AsyncBasicAdapter, AsyncContextOutcomeStream, AsyncContextStream,
    AsyncNeighborStream, EdgeParameters, Typename,
};
use trustfall::{FieldValue, QueryResultStream, Schema};

const SCHEMA: &str = r"
schema { query: Root }

type Root {
  Item: [CodeItem!]!
  Project: [CodeItem!]!
  Declaration: [CodeItem!]!
}

type CodeItem {
  id: String!
  kind: String!
  coordinate: String!
  name: String!
  signature: String
  documentation: String!
  score: Int
  state: String!
  revision: String!
  project: [CodeItem!]!
  parent: [CodeItem!]!
  children: [CodeItem!]!
  related: [CodeItem!]!
  sameProject: [CodeItem!]!
}
";

static PARSED_SCHEMA: OnceLock<Result<Schema, String>> = OnceLock::new();

/// Failure to parse or execute a query against an immutable product view.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("Trustfall view query failed: {0}")]
pub struct QueryError(String);

/// Lazy async rows. The stream owns the immutable view arrangement and does
/// not borrow a mutable engine or rebuild graph indexes between resolver calls.
pub type ViewQueryStream = QueryResultStream<'static>;

/// Returns the exact schema accepted by [`execute_view_query`].
///
/// # Errors
///
/// Returns the cached schema parse diagnostic if the embedded schema is invalid.
pub fn schema() -> Result<&'static Schema, QueryError> {
    PARSED_SCHEMA
        .get_or_init(|| Schema::parse(SCHEMA).map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|error| QueryError(error.clone()))
}

/// Executes a lazy asynchronous Trustfall query over one immutable view root.
///
/// The adapter builds one compact adjacency arrangement per execution and all
/// yielded vertices share it through `Arc`. Row values remain in the original
/// `ViewRoot`; resolvers clone only the requested scalar into Trustfall.
///
/// # Errors
///
/// Returns a parse, validation, or query-argument diagnostic before the stream
/// is exposed.
pub fn execute_view_query(
    view: ViewRoot,
    query: &str,
    variables: BTreeMap<String, FieldValue>,
) -> Result<ViewQueryStream, QueryError> {
    let adapter = Arc::new(ViewAdapter::new(view));
    trustfall::execute_query_async(schema()?, adapter, query, variables)
        .map_err(|error| QueryError(error.to_string()))
}

#[derive(Debug)]
struct ViewGraph {
    view: ViewRoot,
    all: Arc<[usize]>,
    projects: Arc<[usize]>,
    declarations: Arc<[usize]>,
    by_id: BTreeMap<RowId, usize>,
    children: BTreeMap<RowId, Arc<[usize]>>,
    project_members: BTreeMap<PackageKey, Arc<[usize]>>,
}

impl ViewGraph {
    fn new(view: ViewRoot) -> Self {
        let mut projects = Vec::new();
        let mut declarations = Vec::new();
        let mut by_id = BTreeMap::new();
        let mut children = BTreeMap::<RowId, Vec<usize>>::new();
        let mut project_members = BTreeMap::<PackageKey, Vec<usize>>::new();
        for (index, row) in view.rows().iter().enumerate() {
            by_id.insert(row.id, index);
            match row.id {
                RowId::Package(_) => projects.push(index),
                RowId::Symbol(_) => declarations.push(index),
                RowId::Object(_) => {}
            }
            if let Some(package) = row.package {
                project_members.entry(package).or_default().push(index);
            }
        }
        for (index, row) in view.rows().iter().enumerate() {
            if let Some(parent) = row.parent {
                children
                    .entry(RowId::Symbol(parent))
                    .or_default()
                    .push(index);
            } else if let Some(package) = row.package {
                children
                    .entry(RowId::Package(package))
                    .or_default()
                    .push(index);
            }
        }
        Self {
            all: (0..view.rows().len()).collect::<Vec<_>>().into(),
            projects: projects.into(),
            declarations: declarations.into(),
            by_id,
            children: children
                .into_iter()
                .map(|(key, values)| (key, values.into()))
                .collect(),
            project_members: project_members
                .into_iter()
                .map(|(key, values)| (key, values.into()))
                .collect(),
            view,
        }
    }
}

#[derive(Clone, Debug)]
struct Vertex {
    graph: Arc<ViewGraph>,
    index: usize,
}

impl Vertex {
    fn row(&self) -> &Row {
        &self.graph.view.rows()[self.index]
    }

    fn one(&self, id: Option<RowId>) -> Arc<[usize]> {
        id.and_then(|id| self.graph.by_id.get(&id).copied())
            .map_or_else(|| Arc::from([]), |index| Arc::from([index]))
    }

    fn neighbors(&self, edge: &str) -> Arc<[usize]> {
        let row = self.row();
        match edge {
            "project" => self.one(row.package.map(RowId::Package)),
            "parent" => self.one(row.parent.map(RowId::Symbol)),
            "children" => self
                .graph
                .children
                .get(&row.id)
                .cloned()
                .unwrap_or_else(|| Arc::from([])),
            "sameProject" => row
                .package
                .or(match row.id {
                    RowId::Package(package) => Some(package),
                    RowId::Symbol(_) | RowId::Object(_) => None,
                })
                .and_then(|package| self.graph.project_members.get(&package).cloned())
                .unwrap_or_else(|| Arc::from([])),
            "related" => self.related(),
            _ => Arc::from([]),
        }
    }

    fn related(&self) -> Arc<[usize]> {
        let row = self.row();
        let mut related = BTreeSet::new();
        if let Some(package) = row.package
            && let Some(index) = self.graph.by_id.get(&RowId::Package(package))
        {
            related.insert(*index);
        }
        if let Some(parent) = row.parent
            && let Some(index) = self.graph.by_id.get(&RowId::Symbol(parent))
        {
            related.insert(*index);
        }
        if let Some(children) = self.graph.children.get(&row.id) {
            related.extend(children.iter().copied());
        }
        related.into_iter().collect::<Vec<_>>().into()
    }

    fn stream(graph: Arc<ViewGraph>, indices: Arc<[usize]>) -> AsyncNeighborStream<'static, Self> {
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

#[derive(Debug)]
struct ViewAdapter {
    graph: Arc<ViewGraph>,
}

impl ViewAdapter {
    fn new(view: ViewRoot) -> Self {
        Self {
            graph: Arc::new(ViewGraph::new(view)),
        }
    }
}

impl AsyncBasicAdapter<'static> for ViewAdapter {
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
                vertex.row().id.stable_key().into()
            }),
            "kind" => resolve_property_with(contexts, |vertex: &Vertex| {
                match vertex.row().id {
                    RowId::Package(_) => "project",
                    RowId::Symbol(_) => "declaration",
                    RowId::Object(_) => "object",
                }
                .into()
            }),
            "coordinate" => resolve_property_with(contexts, |vertex: &Vertex| {
                vertex.row().label.as_str().into()
            }),
            "name" => resolve_property_with(contexts, |vertex: &Vertex| {
                display_name(&vertex.row().label).into()
            }),
            "signature" => resolve_property_with(contexts, |vertex: &Vertex| {
                vertex
                    .row()
                    .signature
                    .as_deref()
                    .map_or(FieldValue::Null, Into::into)
            }),
            "documentation" => resolve_property_with(contexts, |vertex: &Vertex| {
                fragments_text(&vertex.row().document).into()
            }),
            "score" => resolve_property_with(contexts, |vertex: &Vertex| {
                vertex
                    .row()
                    .score
                    .map_or(FieldValue::Null, |score| u64::from(score).into())
            }),
            "state" => resolve_property_with(contexts, |vertex: &Vertex| {
                format!("{:?}", vertex.row().state).to_lowercase().into()
            }),
            "revision" => resolve_property_with(contexts, |vertex: &Vertex| {
                encode_id(vertex.row().basis.root.as_bytes()).into()
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
            "project" => resolve_neighbors_with(contexts, |vertex: &Vertex| {
                Vertex::stream(Arc::clone(&vertex.graph), vertex.neighbors("project"))
            }),
            "parent" => resolve_neighbors_with(contexts, |vertex: &Vertex| {
                Vertex::stream(Arc::clone(&vertex.graph), vertex.neighbors("parent"))
            }),
            "children" => resolve_neighbors_with(contexts, |vertex: &Vertex| {
                Vertex::stream(Arc::clone(&vertex.graph), vertex.neighbors("children"))
            }),
            "related" => resolve_neighbors_with(contexts, |vertex: &Vertex| {
                Vertex::stream(Arc::clone(&vertex.graph), vertex.neighbors("related"))
            }),
            "sameProject" => resolve_neighbors_with(contexts, |vertex: &Vertex| {
                Vertex::stream(Arc::clone(&vertex.graph), vertex.neighbors("sameProject"))
            }),
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

fn display_name(coordinate: &str) -> &str {
    coordinate.rsplit("::").next().unwrap_or(coordinate)
}

fn fragments_text(fragments: &[Fragment]) -> String {
    let mut output = String::new();
    for fragment in fragments {
        match fragment {
            Fragment::Text(text) | Fragment::Code(text) => output.push_str(text),
            Fragment::Link { label, .. } => output.push_str(label),
            Fragment::Break => output.push('\n'),
        }
    }
    output
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use backend_library::{
        Basis, Frontier, Row, ViewRoot, object_version, package_key, symbol_key, view_key,
        view_state_root,
    };
    use futures_util::StreamExt as _;

    fn view() -> ViewRoot {
        let source = view_state_root(&[]);
        let basis = Basis::new(source, object_version(b"source"));
        let package = package_key("/project");
        let parent = symbol_key("/project::src/lib.rs");
        let child = symbol_key("/project::src/lib.rs:2::ferris");
        let rows = vec![
            Row::new(RowId::Package(package), basis, "/project"),
            Row::in_package(
                RowId::Symbol(parent),
                basis,
                package,
                "/project::src/lib.rs",
            ),
            Row::in_package(
                RowId::Symbol(child),
                basis,
                package,
                "/project::src/lib.rs:2::ferris",
            )
            .with_parent(parent)
            .with_signature("pub fn ferris()"),
        ];
        ViewRoot::new_incomplete(
            view_key(b"trustfall-view"),
            basis,
            Frontier::new(basis.branch, basis.log, basis.schema, source, 3),
            rows,
            Vec::new(),
        )
        .expect("view")
    }

    #[test]
    fn async_engine_queries_properties_and_adjacency_from_one_arrangement() {
        let query = r#"{
            Declaration {
                name @filter(op: "=", value: ["$selected"])
                coordinate @output
                parent { parent: name @output }
                project { project: coordinate @output }
            }
        }"#;
        let variables = BTreeMap::from([("selected".to_owned(), FieldValue::from("ferris"))]);
        let stream = execute_view_query(view(), query, variables).expect("query");
        let rows: Vec<trustfall::QueryResult> =
            futures_executor::block_on(stream.collect::<Vec<trustfall::QueryResult>>());
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0]["coordinate"],
            "/project::src/lib.rs:2::ferris".into()
        );
        assert_eq!(rows[0]["parent"], "src/lib.rs".into());
        assert_eq!(rows[0]["project"], "/project".into());
    }

    #[test]
    fn query_variables_are_admitted_by_trustfall_before_streaming() {
        let query = r#"{
            Declaration {
                name @filter(op: "=", value: ["$selected"])
                signature @output
            }
        }"#;
        let variables = BTreeMap::from([("selected".to_owned(), FieldValue::from("ferris"))]);
        let stream = execute_view_query(view(), query, variables).expect("query");
        let rows: Vec<trustfall::QueryResult> =
            futures_executor::block_on(stream.collect::<Vec<trustfall::QueryResult>>());
        assert_eq!(rows[0]["signature"], "pub fn ferris()".into());
    }
}
