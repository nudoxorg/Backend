use std::sync::Arc;

use serde::Serialize;
use serde_json::Value as JsonValue;

#[derive(Debug, Clone, Serialize)]
pub struct GraphNode {
	pub uri:      Arc<str>,
	pub document: Arc<JsonValue>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphEdge {
	pub source:   Arc<str>,
	pub target:   Arc<str>,
	pub relation: Arc<str>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct GraphResponse {
	pub nodes: Vec<GraphNode>,
	pub edges: Vec<GraphEdge>,
}
