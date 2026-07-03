//! Read handlers: precise search (default), gated semantic search, package
//! search, and graph expand. Paged responses use the shared [`heart::Page`]
//! (whose items are `heart::Scored<T>`); the streaming `/search` emits those
//! hits as NDJSON so a large result set never materializes server-side.

use std::sync::Arc;

use axum::{Json, extract::State, response::Response};
use heart::{Page, Symbol};

use runtime::vector::EmbeddingModel;
use crate::Server;
use crate::error::ServerResult;
use crate::http::dto::SearchRequestDto;

/// `POST /search` — precise-or-planned symbol search. Streams NDJSON pages of
/// `heart::Scored<Symbol>`.
pub async fn search<M: EmbeddingModel>(
	State(server): State<Arc<Server<M>>>,
	Json(req): Json<SearchRequestDto>,
) -> ServerResult<Response> {
	let _ = (server, req);
	todo!("build AccessContext + Search from req, call search_symbols, stream NDJSON of Scored<Symbol>")
}

/// `POST /packages/search` — registry (package) search.
pub async fn search_packages<M: EmbeddingModel>(
	State(server): State<Arc<Server<M>>>,
	Json(req): Json<SearchRequestDto>,
) -> ServerResult<Json<Page<registry::GlobalPackage>>> {
	let _ = (server, req);
	todo!("registry search, collect one page into heart::Page<GlobalPackage>")
}

/// `POST /expand` — walk graph relationships from a hit, merging into the
/// caller's session graph.
pub async fn expand<M: EmbeddingModel>(
	State(server): State<Arc<Server<M>>>,
	Json(req): Json<SearchRequestDto>,
) -> ServerResult<Json<Page<Symbol>>> {
	let _ = (server, req);
	todo!("resolve hit, Server::expand, project results into heart::Page<Symbol>")
}
