use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use terminusdb_client::{BranchSpec, TerminusDBHttpClient};

use crate::http::error::AppError;

use super::fetch_document;
use super::session::SessionGraphState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
	pub uri:      String,
	pub document: JsonValue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {
	pub source:   String,
	pub target:   String,
	pub relation: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct GraphResponse {
	pub nodes: Vec<GraphNode>,
	pub edges: Vec<GraphEdge>,
}

pub(crate) async fn build_graph(
	client: &TerminusDBHttpClient,
	spec: &BranchSpec,
	roots: Vec<String>,
	depth: usize,
	breadth: usize,
) -> Result<SessionGraphState, AppError> {
	let mut graph = SessionGraphState::default();
	let mut cache = HashMap::<String, Option<JsonValue>>::new();
	let mut visited = HashSet::<String>::new();
	let mut queue: VecDeque<(String, usize)> =
		roots.into_iter().map(|uri| (uri, depth)).collect();

	while let Some((uri, remaining_depth)) = queue.pop_front() {
		if !visited.insert(uri.clone()) {
			continue;
		}

		let Some(document) = fetch_document(client, spec, &mut cache, &uri).await? else {
			continue;
		};
		graph.add_node(uri.clone(), document.clone());

		let links = extract_links(&document, breadth);
		for (relation, target) in links {
			graph.add_edge(uri.clone(), target.clone(), relation);
			if remaining_depth > 0 {
				queue.push_back((target, remaining_depth - 1));
			}
		}
	}

	Ok(graph)
}

fn extract_links(document: &JsonValue, breadth: usize) -> Vec<(String, String)> {
	let mut seen = BTreeSet::<(String, String)>::new();
	collect_links(document, "", &mut seen);
	seen.into_iter().take(breadth).collect()
}

fn collect_links(value: &JsonValue, relation: &str, out: &mut BTreeSet<(String, String)>) {
	match value {
		JsonValue::Object(map) => {
			if let Some(id) = map.get("@id").and_then(JsonValue::as_str)
				&& looks_like_internal_uri(id)
			{
				out.insert((relation_name(relation), id.to_owned()));
				return;
			}

			for (key, nested) in map {
				if key == "@context" || key == "@type" {
					continue;
				}
				collect_links(nested, key, out);
			}
		}
		JsonValue::Array(values) => {
			for nested in values {
				collect_links(nested, relation, out);
			}
		}
		JsonValue::String(text) => {
			if looks_like_internal_uri(text) {
				out.insert((relation_name(relation), text.to_owned()));
			}
		}
		_ => {}
	}
}

fn looks_like_internal_uri(value: &str) -> bool {
	if value.is_empty() || value.starts_with("http://") || value.starts_with("https://") {
		return false;
	}

	let Some((prefix, _rest)) = value.split_once('/') else {
		return false;
	};

	prefix.chars().next().is_some_and(|ch| ch.is_ascii_uppercase())
}

fn relation_name(relation: &str) -> String {
	if relation.is_empty() { "reference".to_owned() } else { relation.to_owned() }
}
