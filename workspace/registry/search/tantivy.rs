//! Replica-local tantivy index over the searchable package projection.
//!
//! Schema v4 (SCHEMA_VERSION = 4):
//! - `package_id`  STRING|STORED — upsert/delete key
//! - `name_exact`  STRING        — search_surface lowercased + canonical lowercased
//!   + original lowercased (multiple values per doc)
//! - `name_tokens` TEXT(ident)   — search_surface + original, identifier-tokenized
//! - `name_ns`     TEXT(ident)   — namespace segments space-joined
//! - `description` TEXT          — from SearchFacets.description (S2)
//! - `keywords`    TEXT          — from SearchFacets.keyword_text() plus
//!   index-time name separator parts ([`super::ranking::enrich`]; no separate `extra` field)
//! - `ecosystem`   STRING|STORED — language token (Must filter, Q4)
//! - `record`      STORED        — full serialized GlobalPackage (hydrate)
//! - `deps`        STRING        — one value per facets.dependencies slug (multi-valued)
//! - `license`     STRING        — facets.license as a single lowercase term, when present
//! - `repo`        STRING        — facets.repo_slug, when present
//! - `quality_ppm`         FAST u64 — facets.quality_ppm (0 if unknown)
//! - `downloads`           FAST u64 — facets.downloads (0 if None)
//! - `popularity_pct_ppm`  FAST u64 — eco CDF × 1e6 (0 if None / not yet filled)
//!
//! FAST ranking fields are for future collectors and debug; the live rank path
//! still hydrates the full `record` JSON. Query matching is unchanged from v3.
//!
//! The IdentifierTokenizer from registry/runtime/text/tokenizer.rs is
//! re-registered on this index after every open (same mechanism as TextIndex).

use heart::PackageId;
use tantivy::{Index, IndexReader, schema::Field};

use crate::{
	GlobalPackage,
	error::SearchError,
	schema::codec,
};
use crate::runtime::text::tokenizer;
use super::structured::StructuredQuery;

/// Schema version written to `schema_version` next to the index dir.
/// Mismatch on open → wipe contents + reset watermark to 0.
const SCHEMA_VERSION: u32 = 4;

/// Filename for the durable sync watermark.
const WATERMARK_FILE: &str = "sync_watermark.json";

/// Filename for the schema version marker.
const SCHEMA_VERSION_FILE: &str = "schema_version";

/// The last postgres position a replica has folded into its local index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SyncWatermark {
	pub position: i64,
}

/// A replica-local tantivy index over the searchable package projection.
pub struct PackageIndex {
	index:     Index,
	reader:    IndexReader,
	watermark: SyncWatermark,
	dir:       std::path::PathBuf,
}

/// Ranking signals stored as FAST u64 columns (schema v4).
///
/// Read via [`PackageIndex::fast_ranking_signals`] for collectors / debug /
/// round-trip tests. Live ranking still uses hydrated facets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FastRankingSignals {
	/// Quality in parts-per-million (`0..=1_000_000`); `0` if facets unknown.
	pub quality_ppm: u64,
	/// Monthly downloads; `0` when facets omit downloads (`None`).
	pub downloads: u64,
	/// Ecosystem popularity percentile as ppm (`0..=1_000_000`); `0` if unset.
	pub popularity_pct_ppm: u64,
}

/// Schema field handles — resolved once per operation so writer/reader agree.
struct Fields {
	package_id:  Field,
	name_exact:  Field,
	name_tokens: Field,
	name_ns:     Field,
	description: Field,
	keywords:    Field,
	ecosystem:   Field,
	record:      Field,
	/// One value per dependency slug (multi-valued STRING).
	deps:        Field,
	/// Lowercased license expression, when present.
	license:     Field,
	/// `host/owner/repo` slug, when present.
	repo:        Field,
	/// v4 FAST ranking columns.
	quality_ppm:        Field,
	downloads:          Field,
	popularity_pct_ppm: Field,
}

/// All query boost values in one place so tuning is one-line. (§8.3)
struct PackageQueryBoosts;

impl PackageQueryBoosts {
	const EXACT:       f32 = 4.0;
	const NAME_TOKENS: f32 = 2.0;
	const NAME_NS:     f32 = 1.5;
	const DESCRIPTION: f32 = 1.0;
	const KEYWORDS:    f32 = 1.2;
	const FUZZY:       f32 = 0.3;
	/// Boost applied to synonym-expanded keyword matches.
	const EXPANDED:    f32 = 0.5;
}

/// Tantivy writer heap per sync batch.
const WRITER_HEAP_BYTES: usize = 50 << 20;

/// Postgres batch size per sync poll.
const SYNC_BATCH: u64 = 1024;

impl PackageIndex {
	/// Open (or create) the replica-local index at `path`.
	///
	/// On schema version mismatch or missing marker with non-empty index,
	/// the index directory contents are wiped and the watermark is reset to
	/// zero so a full resync rebuilds the index under the new schema.
	pub fn open(path: &std::path::Path) -> Result<Self, SearchError> {
		std::fs::create_dir_all(path)
			.map_err(|e| SearchError::Tantivy(tantivy::TantivyError::from(e)))?;

		// Schema-version check: wipe + resync when the on-disk marker is missing
		// or stale (§4.3.5).
		Self::maybe_wipe_for_schema_change(path)?;

		let directory = tantivy::directory::MmapDirectory::open(path)
			.map_err(|e| SearchError::Tantivy(e.into()))?;
		let index =
			Index::open_or_create(directory, Self::schema()).map_err(SearchError::Tantivy)?;
		// Register the identifier tokenizer (same mechanism as TextIndex::open_or_create).
		tokenizer::register(&index);
		let reader = index.reader().map_err(SearchError::Tantivy)?;
		let mut watermark = load_watermark(path);
		if watermark.position > 0 {
			let _ = reader.reload();
			if reader.searcher().num_docs() == 0 {
				tracing::warn!(
					path = %path.display(),
					position = watermark.position,
					"watermark present but index empty; resuming from zero"
				);
				watermark = SyncWatermark { position: 0 };
			}
		}
		tracing::debug!(
			path = %path.display(),
			position = watermark.position,
			schema_version = SCHEMA_VERSION,
			"replica-local package index opened"
		);
		Ok(Self { index, reader, watermark, dir: path.to_path_buf() })
	}

	/// Wipe the index directory contents and reset the watermark when the
	/// on-disk schema version does not match `SCHEMA_VERSION`.
	fn maybe_wipe_for_schema_change(path: &std::path::Path) -> Result<(), SearchError> {
		let marker_path = path.join(SCHEMA_VERSION_FILE);
		let on_disk = std::fs::read_to_string(&marker_path).ok()
			.and_then(|s| s.trim().parse::<u32>().ok());

		let needs_wipe = match on_disk {
			Some(v) if v == SCHEMA_VERSION => false,
			_ => {
				// Check if there are any actual index files to wipe.
				let has_content = std::fs::read_dir(path)
					.map(|mut rd| rd.next().is_some())
					.unwrap_or(false);
				// Only wipe when the marker is wrong/missing AND there is content.
				// A fresh empty dir is fine to just write the marker into.
				has_content && on_disk != Some(SCHEMA_VERSION)
			}
		};

		if needs_wipe {
			tracing::warn!(
				path = %path.display(),
				on_disk = ?on_disk,
				current = SCHEMA_VERSION,
				"schema version mismatch; wiping index for resync"
			);
			// Remove all files inside the directory (not the dir itself).
			if let Ok(entries) = std::fs::read_dir(path) {
				for entry in entries.flatten() {
					let _ = std::fs::remove_file(entry.path());
				}
			}
			// Reset watermark file.
			let wm_path = path.join(WATERMARK_FILE);
			let _ = std::fs::remove_file(&wm_path);
		}

		// Write the current schema version marker (fresh or post-wipe).
		if std::fs::write(&marker_path, SCHEMA_VERSION.to_string()).is_err() {
			tracing::warn!(path = %marker_path.display(), "failed to write schema_version marker");
		}

		Ok(())
	}

	/// Schema v4: all fields (v3 text/facet filters + FAST ranking columns).
	pub fn schema() -> tantivy::schema::Schema {
		use tantivy::schema::{
			FAST, STORED, STRING, TEXT, TextFieldIndexing, TextOptions, IndexRecordOption,
		};
		let mut builder = tantivy::schema::Schema::builder();
		builder.add_text_field("package_id", STRING | STORED);
		// Exact-match field: stores search_surface, canonical, and original
		// (all lowercased) as separate values for O(1) TermQuery lookup.
		builder.add_text_field("name_exact", STRING);
		// Subtoken fields: identifier-tokenized so `mux` finds `gorilla/mux`.
		let ident_options = TextOptions::default().set_indexing_options(
			TextFieldIndexing::default()
				.set_tokenizer(tokenizer::IDENT_TOKENIZER)
				.set_index_option(IndexRecordOption::WithFreqsAndPositions),
		);
		builder.add_text_field("name_tokens", ident_options.clone());
		builder.add_text_field("name_ns", ident_options);
		builder.add_text_field("description", TEXT);
		builder.add_text_field("keywords", TEXT);
		builder.add_text_field("ecosystem", STRING | STORED);
		builder.add_text_field("record", STORED);
		// v3: facet filter fields — multi-valued STRING for deps; single STRING
		// for license and repo.
		builder.add_text_field("deps", STRING);
		builder.add_text_field("license", STRING);
		builder.add_text_field("repo", STRING);
		// v4: FAST ranking signals for collectors / debug (not used by query tree).
		builder.add_u64_field("quality_ppm", FAST);
		builder.add_u64_field("downloads", FAST);
		builder.add_u64_field("popularity_pct_ppm", FAST);
		builder.build()
	}

	fn fields(&self) -> Result<Fields, SearchError> {
		let schema = self.index.schema();
		let field = |name| schema.get_field(name).map_err(SearchError::Tantivy);
		Ok(Fields {
			package_id:  field("package_id")?,
			name_exact:  field("name_exact")?,
			name_tokens: field("name_tokens")?,
			name_ns:     field("name_ns")?,
			description: field("description")?,
			keywords:    field("keywords")?,
			ecosystem:   field("ecosystem")?,
			record:      field("record")?,
			deps:        field("deps")?,
			license:     field("license")?,
			repo:        field("repo")?,
			quality_ppm:        field("quality_ppm")?,
			downloads:          field("downloads")?,
			popularity_pct_ppm: field("popularity_pct_ppm")?,
		})
	}

	/// Fold a batch of package records into the local index at `position`.
	pub fn absorb<'r>(
		&mut self,
		records: impl IntoIterator<Item = &'r GlobalPackage>,
		position: i64,
	) -> Result<SyncWatermark, SearchError> {
		let fields = self.fields()?;
		let mut writer = self.index.writer(WRITER_HEAP_BYTES).map_err(SearchError::Tantivy)?;

		let mut folded = 0usize;
		for record in records {
			let token = record.id.to_string();
			writer.delete_term(tantivy::Term::from_field_text(fields.package_id, &token));

			let json = serde_json::to_string(record)
				.map_err(|e| SearchError::JsonDecode { domain: "GlobalPackage", source: e })?;
			let coordinates = &record.package.coordinates;
			let name = &coordinates.name;

			let structured = name.structured();
			let canonical_lower = name.canonical().to_ascii_lowercase();
			let original_lower  = name.original().to_ascii_lowercase();
			// search_surface: namespace + name WITHOUT authority (R4 — `github`/
			// `com` never become tokens).
			let search_surface = structured.search_surface();
			let search_surface_lower = search_surface.to_ascii_lowercase();
			let namespace_text = structured.namespace.join(" ");

			let mut document = tantivy::TantivyDocument::default();
			document.add_text(fields.package_id, &token);

			// name_exact: store all three lowercased forms as separate values.
			document.add_text(fields.name_exact, &search_surface_lower);
			if canonical_lower != search_surface_lower {
				document.add_text(fields.name_exact, &canonical_lower);
			}
			if original_lower != search_surface_lower && original_lower != canonical_lower {
				document.add_text(fields.name_exact, &original_lower);
			}

			// name_tokens: search_surface ONLY — tokenizing the original would
			// re-admit authority tokens for Go module paths (R4). Full-string
			// lookups on the original are served by name_exact above.
			document.add_text(fields.name_tokens, &search_surface);

			// name_ns: namespace segments space-joined.
			if !namespace_text.is_empty() {
				document.add_text(fields.name_ns, &namespace_text);
			}

			document.add_text(fields.ecosystem, coordinates.ecosystem().as_token());

			// Index-time enrichment: name separator parts → keywords TEXT (no
			// schema bump for a dedicated `extra` field — see `enrich` module).
			// Use search_surface so Go authority never re-enters the bag (R4).
			let eco = coordinates.ecosystem();
			let base_keywords = record
				.facets
				.as_ref()
				.map(|f| f.keyword_text())
				.unwrap_or_default();
			let enrichment = super::ranking::enrich::enrich_package_text(
				&search_surface_lower,
				&base_keywords,
				eco,
			);
			let keywords_text = super::ranking::enrich::merge_keywords(&base_keywords, &enrichment);

			// v4 FAST ranking columns — always written so every doc has values.
			// popularity_pct_ppm: facets.popularity_pct (0..=10000) → scale to
			// 0..=1_000_000 for a uniform ppm-style FAST column.
			let (quality_ppm, downloads, popularity_pct_ppm) = match &record.facets {
				Some(facets) => (
					u64::from(facets.quality_ppm),
					facets.downloads.unwrap_or(0),
					facets
						.popularity_pct
						.map(|p| u64::from(p.min(10_000)) * 100)
						.unwrap_or(0),
				),
				None => (0, 0, 0),
			};
			document.add_u64(fields.quality_ppm, quality_ppm);
			document.add_u64(fields.downloads, downloads);
			document.add_u64(fields.popularity_pct_ppm, popularity_pct_ppm);

			if let Some(facets) = &record.facets {
				if let Some(desc) = &facets.description {
					document.add_text(fields.description, desc.as_str());
				}
				if !keywords_text.is_empty() {
					document.add_text(fields.keywords, &keywords_text);
				}

				// v3 facet filter fields.
				for dep in &facets.dependencies {
					document.add_text(fields.deps, dep.as_str());
				}
				if let Some(lic) = &facets.license {
					document.add_text(fields.license, lic.as_str());
				}
				if let Some(repo) = &facets.repo_slug {
					document.add_text(fields.repo, repo.as_str());
				}
			} else if !keywords_text.is_empty() {
				// No facets yet: still index name-derived extras so dash-split
				// keyword matching works before rich metadata lands.
				document.add_text(fields.keywords, &keywords_text);
			}

			document.add_text(fields.record, &json);
			writer.add_document(document).map_err(SearchError::Tantivy)?;
			folded += 1;
		}

		writer.commit().map_err(SearchError::Tantivy)?;
		self.reader.reload().map_err(SearchError::Tantivy)?;
		self.watermark = SyncWatermark { position };
		persist_watermark(&self.dir, self.watermark);
		tracing::debug!(folded, position, "package records absorbed into replica index");
		Ok(self.watermark)
	}

	/// Poll the catalog outbox (Text sink) from the current watermark and fold
	/// new/changed packages. The watermark position is the outbox `seq` (it was
	/// a postgres timestamp before the catalog port; both are monotonic i64s,
	/// so persisted watermarks stay decodable — a stale timestamp value simply
	/// reads as "far ahead" and the next full rebuild resets it).
	#[tracing::instrument(skip_all, fields(from = self.watermark.position))]
	pub async fn sync_from<Engine: index::engine::VersioningEngine + Send + Sync>(
		&mut self,
		global: &crate::index::GlobalStore<Engine>,
		outbox: &crate::coordination::Outbox<Engine>,
	) -> Result<SyncWatermark, SearchError> {
		let entries = outbox
			.read_since(
				crate::coordination::SinkKind::Text,
				crate::coordination::OutboxSeq(self.watermark.position),
				SYNC_BATCH as usize,
			)
			.await
			.map_err(|error| SearchError::OutboxRead { detail: error.to_string() })?;

		let mut head = self.watermark.position;
		let mut records = Vec::with_capacity(entries.len());
		for entry in &entries {
			head = head.max(entry.id.0);
			match global.get(entry.package).await {
				Ok(record) => records.push(record),
				// A version row can trail its outbox intent (or be tombstoned);
				// skip and let the next intent re-deliver it.
				Err(crate::error::IndexError::NotFound { .. }) => continue,
				Err(error) => {
					return Err(SearchError::OutboxRead { detail: error.to_string() });
				}
			}
		}
		self.absorb(records.iter(), head)
	}

	/// Execute a raw-text query (backward-compat for existing tests/callers).
	/// Delegates to [`Self::query_structured`] via `StructuredQuery::parse`.
	pub fn query(
		&self,
		text: &str,
		limit: usize,
	) -> Result<Vec<(PackageId, f32)>, SearchError> {
		let sq = StructuredQuery::parse(text, None);
		self.query_structured(&sq, limit)
	}

	/// Execute a structured query, returning matching package ids with raw tantivy scores.
	///
	/// Builds a hand-written BooleanQuery tree (§8.3). No tantivy QueryParser is
	/// used — terms are raw, no grammar escaping needed (Q1 fix).
	pub fn query_structured(
		&self,
		sq: &StructuredQuery,
		limit: usize,
	) -> Result<Vec<(PackageId, f32)>, SearchError> {
		use tantivy::{
			Term,
			collector::TopDocs,
			query::{BooleanQuery, BoostQuery, FuzzyTermQuery, Occur, PhraseQuery, TermQuery},
			schema::IndexRecordOption,
		};

		let fields = self.fields()?;
		let searcher = self.reader.searcher();

		// Produce subtokens from the identifier tokenizer (same split as index time).
		// Drop tokens of length < 2 from Must-conjunctions (T2).
		let terms_lower = sq.terms.to_ascii_lowercase();
		let all_subtokens = tokenizer::subtokens(&sq.terms);
		let filtered_subtokens: Vec<String> = all_subtokens
			.iter()
			.filter(|t| t.len() >= 2)
			.cloned()
			.collect();

		let mut outer: Vec<(Occur, Box<dyn tantivy::query::Query>)> = Vec::new();

		// AllQuery fires when there are no free terms — dep:/license:/namespace
		// filters are separate Must clauses and compose naturally with AllQuery.
		if sq.terms.is_empty() {
			outer.push((Occur::Must, Box::new(tantivy::query::AllQuery)));
		} else {
			// Should-tier: all tiers are Should; at least one must match.
			let mut should: Vec<(Occur, Box<dyn tantivy::query::Query>)> = Vec::new();

			// Tier 1: exact match on name_exact (full terms lowercased), boost 4.0.
			let exact_term = Term::from_field_text(fields.name_exact, &terms_lower);
			let exact_q = TermQuery::new(exact_term, IndexRecordOption::Basic);
			should.push((
				Occur::Should,
				Box::new(BoostQuery::new(Box::new(exact_q), PackageQueryBoosts::EXACT)),
			));

			// Tier 2: Must-conjunction of subtokens on name_tokens, boost 2.0.
			if !filtered_subtokens.is_empty() {
				let clauses: Vec<(Occur, Box<dyn tantivy::query::Query>)> = filtered_subtokens
					.iter()
					.map(|t| {
						let term = Term::from_field_text(fields.name_tokens, t);
						let tq: Box<dyn tantivy::query::Query> =
							Box::new(TermQuery::new(term, IndexRecordOption::WithFreqs));
						(Occur::Must, tq)
					})
					.collect();
				let conj = BooleanQuery::new(clauses);
				should.push((
					Occur::Should,
					Box::new(BoostQuery::new(Box::new(conj), PackageQueryBoosts::NAME_TOKENS)),
				));
			}

			// Tier 3: Must-conjunction on name_ns, boost 1.5.
			if !filtered_subtokens.is_empty() {
				let ns_clauses: Vec<(Occur, Box<dyn tantivy::query::Query>)> = filtered_subtokens
					.iter()
					.map(|t| {
						let term = Term::from_field_text(fields.name_ns, t);
						let tq: Box<dyn tantivy::query::Query> =
							Box::new(TermQuery::new(term, IndexRecordOption::WithFreqs));
						(Occur::Must, tq)
					})
					.collect();
				let ns_conj = BooleanQuery::new(ns_clauses);
				should.push((
					Occur::Should,
					Box::new(BoostQuery::new(Box::new(ns_conj), PackageQueryBoosts::NAME_NS)),
				));
			}

			// Tier 4: Must-conjunction on description, boost 1.0.
			if !filtered_subtokens.is_empty() {
				let desc_clauses: Vec<(Occur, Box<dyn tantivy::query::Query>)> = filtered_subtokens
					.iter()
					.map(|t| {
						let term = Term::from_field_text(fields.description, t);
						let tq: Box<dyn tantivy::query::Query> =
							Box::new(TermQuery::new(term, IndexRecordOption::WithFreqs));
						(Occur::Must, tq)
					})
					.collect();
				let desc_conj = BooleanQuery::new(desc_clauses);
				should.push((
					Occur::Should,
					Box::new(BoostQuery::new(Box::new(desc_conj), PackageQueryBoosts::DESCRIPTION)),
				));
			}

			// Tier 5: Must-conjunction on keywords, boost 1.2.
			if !filtered_subtokens.is_empty() {
				let kw_clauses: Vec<(Occur, Box<dyn tantivy::query::Query>)> = filtered_subtokens
					.iter()
					.map(|t| {
						let term = Term::from_field_text(fields.keywords, t);
						let tq: Box<dyn tantivy::query::Query> =
							Box::new(TermQuery::new(term, IndexRecordOption::WithFreqs));
						(Occur::Must, tq)
					})
					.collect();
				let kw_conj = BooleanQuery::new(kw_clauses);
				should.push((
					Occur::Should,
					Box::new(BoostQuery::new(Box::new(kw_conj), PackageQueryBoosts::KEYWORDS)),
				));
			}

			// Tier 6: FuzzyTermQuery — only for single tokens of len 4..=12 (T1).
			let single_token = {
				let parts: Vec<&str> = sq.terms.split_whitespace().collect();
				parts.len() == 1
			};
			if single_token && terms_lower.len() >= 4 && terms_lower.len() <= 12 {
				// Whole-name typos match against name_exact (raw STRING terms);
				// name_tokens only holds sub-words, so the full query term would
				// never fuzzy-match there. A second clause on name_tokens catches
				// a typo in one sub-word of a multi-part name (`gorila` → `mux`'s
				// `gorilla`). Exact distance-1, transpositions allowed — NOT the
				// prefix variant, which would silently widen matching.
				for field in [fields.name_exact, fields.name_tokens] {
					let term = Term::from_field_text(field, &terms_lower);
					let fuzzy = FuzzyTermQuery::new(term, 1, true);
					should.push((
						Occur::Should,
						Box::new(BoostQuery::new(Box::new(fuzzy), PackageQueryBoosts::FUZZY)),
					));
				}
			}

			// Tier 7: synonym expansions — Should-disjunction on keywords, boost 0.5.
			// Only fires when expand_synonyms has been called.
			if !sq.expanded_terms.is_empty() {
				let exp_clauses: Vec<(Occur, Box<dyn tantivy::query::Query>)> = sq.expanded_terms
					.iter()
					.map(|t| {
						let term = Term::from_field_text(fields.keywords, t);
						let tq: Box<dyn tantivy::query::Query> =
							Box::new(TermQuery::new(term, IndexRecordOption::WithFreqs));
						(Occur::Should, tq)
					})
					.collect();
				let exp_block = BooleanQuery::new(exp_clauses);
				should.push((
					Occur::Should,
					Box::new(BoostQuery::new(Box::new(exp_block), PackageQueryBoosts::EXPANDED)),
				));
			}

			// Wrap the Should tiers so at least one must match.
			let name_block = BooleanQuery::new(should);
			outer.push((Occur::Must, Box::new(name_block)));
		}

		// Q4: Must TermQuery(ecosystem) when known — index-pruned, not post-filter.
		if let Some(eco) = sq.ecosystem {
			let eco_term = Term::from_field_text(fields.ecosystem, eco.as_token());
			outer.push((
				Occur::Must,
				Box::new(TermQuery::new(eco_term, IndexRecordOption::Basic)),
			));
		}

		// Namespace constraint: Must subtoken-conjunction on name_ns.
		if let Some(ns) = &sq.namespace {
			let ns_subtokens: Vec<String> = tokenizer::subtokens(ns)
				.into_iter()
				.filter(|t| t.len() >= 2)
				.collect();
			if !ns_subtokens.is_empty() {
				let ns_clauses: Vec<(Occur, Box<dyn tantivy::query::Query>)> = ns_subtokens
					.iter()
					.map(|t| {
						let term = Term::from_field_text(fields.name_ns, t);
						let tq: Box<dyn tantivy::query::Query> =
							Box::new(TermQuery::new(term, IndexRecordOption::WithFreqs));
						(Occur::Must, tq)
					})
					.collect();
				outer.push((Occur::Must, Box::new(BooleanQuery::new(ns_clauses))));
			}
		}

		// dep: filters — Must TermQuery on the `deps` field for each value.
		for dep_name in &sq.deps {
			let term = Term::from_field_text(fields.deps, dep_name);
			outer.push((
				Occur::Must,
				Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
			));
		}

		// license: filter — Must TermQuery on `license` when present.
		if let Some(lic) = &sq.license {
			let term = Term::from_field_text(fields.license, lic);
			outer.push((
				Occur::Must,
				Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
			));
		}

		// Phrase filters: for each phrase with ≥ 2 subtokens, build a PhraseQuery
		// on `description` AND `keywords` (both TEXT = positions indexed). The two
		// are wrapped as a Should-pair inside one Must BooleanQuery so the phrase
		// must appear in at least one prose field.
		for phrase in &sq.phrases {
			let phrase_subtokens = tokenizer::subtokens(phrase);
			if phrase_subtokens.len() < 2 {
				// < 2 usable subtokens — words are already in terms; skip.
				continue;
			}
			// Build a PhraseQuery for a given field using the shared subtoken list.
			let make_phrase = |field: Field| -> Box<dyn tantivy::query::Query> {
				let offset_terms: Vec<(usize, Term)> = phrase_subtokens
					.iter()
					.enumerate()
					.map(|(pos, t)| (pos, Term::from_field_text(field, t)))
					.collect();
				Box::new(PhraseQuery::new_with_offset(offset_terms))
			};
			let phrase_clauses: Vec<(Occur, Box<dyn tantivy::query::Query>)> = vec![
				(Occur::Should, make_phrase(fields.description)),
				(Occur::Should, make_phrase(fields.keywords)),
			];
			outer.push((Occur::Must, Box::new(BooleanQuery::new(phrase_clauses))));
		}

		let top_query = BooleanQuery::new(outer);
		let top = searcher
			.search(&top_query, &TopDocs::with_limit(limit.max(1)))
			.map_err(SearchError::Tantivy)?;

		top.into_iter()
			.map(|(score, address)| {
				let document: tantivy::TantivyDocument =
					searcher.doc(address).map_err(SearchError::Tantivy)?;
				let id = stored_text(&document, fields.package_id)?
					.parse::<uuid::Uuid>()
					.map_err(|_| SearchError::StoredIdNotUuid)?;
				Ok((codec::package_id_from_uuid(id), score))
			})
			.collect()
	}

	/// Hydrate matched package ids from the stored projection.
	pub async fn hydrate(&self, ids: &[PackageId]) -> Result<Vec<GlobalPackage>, SearchError> {
		use tantivy::{Term, collector::TopDocs, query::TermQuery, schema::IndexRecordOption};

		let fields = self.fields()?;
		let searcher = self.reader.searcher();
		let mut records = Vec::with_capacity(ids.len());
		for id in ids {
			let term = Term::from_field_text(fields.package_id, &id.to_string());
			let query = TermQuery::new(term, IndexRecordOption::Basic);
			let top =
				searcher.search(&query, &TopDocs::with_limit(1)).map_err(SearchError::Tantivy)?;
			let Some((_, address)) = top.into_iter().next() else {
				tracing::debug!(package = %id, "hydrate miss against replica index");
				continue;
			};
			let document: tantivy::TantivyDocument =
				searcher.doc(address).map_err(SearchError::Tantivy)?;
			let json = stored_text(&document, fields.record)?;
			records.push(serde_json::from_str(&json)
				.map_err(|e| SearchError::JsonDecode { domain: "GlobalPackage", source: e })?);
		}
		Ok(records)
	}

	/// Read FAST ranking signals for a package (schema v4).
	///
	/// Locates the doc via `package_id` TermQuery + TopDocs, then reads the
	/// columnar FAST fields from the segment reader. Returns `None` when the
	/// package is not in the index. Used by future collectors, debug, and tests
	/// that prove absorb → FAST round-trip; the live rank path still hydrates
	/// the full stored record.
	pub fn fast_ranking_signals(
		&self,
		id: PackageId,
	) -> Result<Option<FastRankingSignals>, SearchError> {
		use tantivy::{Term, collector::TopDocs, query::TermQuery, schema::IndexRecordOption};

		let fields = self.fields()?;
		let searcher = self.reader.searcher();
		let term = Term::from_field_text(fields.package_id, &id.to_string());
		let query = TermQuery::new(term, IndexRecordOption::Basic);
		let top =
			searcher.search(&query, &TopDocs::with_limit(1)).map_err(SearchError::Tantivy)?;
		let Some((_, address)) = top.into_iter().next() else {
			return Ok(None);
		};

		let segment_reader = searcher.segment_reader(address.segment_ord);
		let fast = segment_reader.fast_fields();
		let quality = fast.u64("quality_ppm").map_err(SearchError::Tantivy)?;
		let downloads = fast.u64("downloads").map_err(SearchError::Tantivy)?;
		let popularity = fast.u64("popularity_pct_ppm").map_err(SearchError::Tantivy)?;

		Ok(Some(FastRankingSignals {
			quality_ppm: quality.first(address.doc_id).unwrap_or(0),
			downloads: downloads.first(address.doc_id).unwrap_or(0),
			popularity_pct_ppm: popularity.first(address.doc_id).unwrap_or(0),
		}))
	}

	/// Snapshot of index health for ops dashboards (doc count, watermark, schema).
	pub fn health_snapshot(&self) -> super::health::PackageIndexHealth {
		super::health::PackageIndexHealth {
			schema_version: SCHEMA_VERSION,
			num_docs: self.reader.searcher().num_docs(),
			watermark_position: self.watermark.position,
			path: self.dir.clone(),
		}
	}

	/// Alias for [`Self::health_snapshot`].
	pub fn health(&self) -> super::health::PackageIndexHealth {
		self.health_snapshot()
	}

	pub fn watermark(&self) -> SyncWatermark { self.watermark }

	/// Remove a package's document from the replica-local index by its id.
	///
	/// Issues a `delete_term` on the `package_id` field and commits immediately
	/// so the removal is visible to the next searcher reload. Analogous to the
	/// `absorb`'s delete-before-add but without the subsequent add.
	///
	/// Called by the outbox consumer on a `Delete` intent — the mirror tombstone
	/// path. Blob / CAS data is retained; only the search projection is removed.
	pub fn remove(&mut self, package: PackageId) -> Result<(), SearchError> {
		let fields = self.fields()?;
		let token = package.to_string();
		let mut writer: tantivy::IndexWriter<tantivy::TantivyDocument> =
			self.index.writer(WRITER_HEAP_BYTES).map_err(SearchError::Tantivy)?;
		writer.delete_term(tantivy::Term::from_field_text(fields.package_id, &token));
		writer.commit().map_err(SearchError::Tantivy)?;
		self.reader.reload().map_err(SearchError::Tantivy)?;
		tracing::debug!(%package, "package removed from replica search index");
		Ok(())
	}
}

fn load_watermark(dir: &std::path::Path) -> SyncWatermark {
	let path = dir.join(WATERMARK_FILE);
	match std::fs::read(&path) {
		Ok(bytes) => match serde_json::from_slice::<SyncWatermark>(&bytes) {
			Ok(wm) => wm,
			Err(e) => {
				tracing::warn!(path = %path.display(), error = %e, "corrupt tantivy watermark; resuming from zero");
				SyncWatermark { position: 0 }
			}
		},
		Err(e) if e.kind() == std::io::ErrorKind::NotFound => SyncWatermark { position: 0 },
		Err(e) => {
			tracing::warn!(path = %path.display(), error = %e, "failed to read tantivy watermark; resuming from zero");
			SyncWatermark { position: 0 }
		}
	}
}

fn persist_watermark(dir: &std::path::Path, watermark: SyncWatermark) {
	let path = dir.join(WATERMARK_FILE);
	let Ok(bytes) = serde_json::to_vec(&watermark) else { return; };
	let tmp = path.with_extension("json.tmp");
	if let Err(e) = std::fs::write(&tmp, &bytes) {
		tracing::warn!(path = %tmp.display(), error = %e, "failed to write tantivy watermark");
		return;
	}
	if let Err(e) = std::fs::rename(&tmp, &path) {
		tracing::warn!(path = %path.display(), error = %e, "failed to persist tantivy watermark");
	}
}

fn stored_text(
	document: &tantivy::TantivyDocument,
	field: Field,
) -> Result<String, SearchError> {
	match document.get_first(field) {
		Some(tantivy::schema::OwnedValue::Str(text)) => Ok(text.clone()),
		_ => Err(SearchError::TantivyInternal(tantivy::TantivyError::InternalError(
			"stored document is missing a schema-required text field".into()
		))),
	}
}

fn codec_to_search(e: codec::CodecError) -> SearchError { SearchError::Codec(e) }

