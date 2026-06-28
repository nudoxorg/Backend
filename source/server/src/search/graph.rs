use std::{collections::{HashMap, HashSet, VecDeque}, sync::Arc};

use serde_json::Value as JsonValue;
use terminusdb_client::{BranchSpec, TerminusDBHttpClient};

pub use search::GraphResponse;
use search::SessionGraphState;

use super::fetch_document;
use crate::http::error::AppError;

pub(crate) async fn build_graph(
	client: &TerminusDBHttpClient,
	spec: &BranchSpec,
	roots: Vec<String>,
	depth: usize,
	breadth: usize,
) -> Result<SessionGraphState, AppError> {
	let mut graph = SessionGraphState::default();
	let mut cache = HashMap::<String, Option<JsonValue>>::new();
	let mut visited = HashSet::<Arc<str>>::new();
	let mut queue: VecDeque<(Arc<str>, usize)> =
		roots.into_iter().map(|uri| (Arc::<str>::from(uri), depth)).collect();

	while let Some((uri, remaining_depth)) = queue.pop_front() {
		if !visited.insert(uri.clone()) {
			continue;
		}

		let Some(document) = fetch_document(client, spec, &mut cache, &uri).await? else {
			continue;
		};
		// Wrap the fetched document once; every later clone is an Arc bump.
		let document = Arc::new(document);
		graph.add_node(uri.clone(), document.clone());

		let links = extract_links(&document, breadth);
		for (relation, target) in links {
			let target: Arc<str> = Arc::from(target);
			graph.add_edge(uri.clone(), target.clone(), Arc::from(relation));
			if remaining_depth > 0 {
				queue.push_back((target, remaining_depth - 1));
			}
		}
	}

	Ok(graph)
}

fn extract_links(document: &JsonValue, breadth: usize) -> Vec<(String, String)> {
	use std::collections::BTreeSet;
	let mut seen = BTreeSet::<(String, String)>::new();
	collect_links(document, "", &mut seen);
	seen.into_iter().take(breadth).collect()
}

fn collect_links(
	value: &JsonValue,
	relation: &str,
	out: &mut std::collections::BTreeSet<(String, String)>,
) {
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
		JsonValue::String(text) if looks_like_internal_uri(text) => {
			out.insert((relation_name(relation), text.to_owned()));
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
