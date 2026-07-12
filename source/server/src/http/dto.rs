use nudox_core::{SymbolMatch, SymbolOrigin};
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
pub struct SymbolMatchResponse {
	pub symbol_name:      String,
	pub occurrence_id:    String,
	pub kind:             Option<String>,
	pub lib_name:         Option<String>,
	pub lib_version:      Option<String>,
	pub repo_id:          Option<String>,
	pub score:            f32,
	pub occurrence_count: usize,
	pub snippet:          String,
}

impl From<SymbolMatch> for SymbolMatchResponse {
	fn from(m: SymbolMatch) -> Self {
		let (lib_name, lib_version, repo_id) = match &m.blob.symbol_origin {
			SymbolOrigin::ExternalLib { lib } => {
				(Some(lib.name.clone()), Some(lib.version.clone()), None)
			}
			SymbolOrigin::Repo { repo_id } => (None, None, Some(repo_id.as_str().to_owned())),
		};
		SymbolMatchResponse {
			symbol_name: m.blob.symbol_name.clone(),
			occurrence_id: m.blob.occurrence_id.to_string(),
			kind: m.blob.kind.map(|k| k.to_string()),
			lib_name,
			lib_version,
			repo_id,
			score: m.score.get(),
			occurrence_count: m.occurrences.len(),
			snippet: m.blob.source.raw_code.to_string(),
		}
	}
}

#[derive(Serialize)]
pub(super) struct HealthResponse {
	pub status:           &'static str,
	pub tracked_packages: usize,
}

#[derive(Deserialize)]
pub(super) struct SearchQuery {
	pub q:     String,
	pub limit: Option<usize>,
}

#[derive(Deserialize)]
pub(super) struct LookupQuery {
	pub q:        Option<String>,
	pub symbol:   Option<String>,
	pub language: Option<String>,
	pub package:  Option<String>,
}

#[derive(Deserialize)]
pub(super) struct RunSearchQuery {
	pub q:       String,
	pub session: Option<String>,
	pub limit:   Option<usize>,
}

#[derive(Deserialize)]
pub(super) struct ExpandQuery {
	pub uri:     String,
	pub session: Option<String>,
	pub depth:   Option<usize>,
	pub breadth: Option<usize>,
}

#[derive(Deserialize)]
pub(super) struct SessionQuery {
	pub session: String,
}
