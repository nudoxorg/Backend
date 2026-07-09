//! Our Tantivy abstraction against the backing registry
//!
//! We prefer Tantivy, not as a source of truth, but for its search abilities.
//! This forms a replica structure that gets a searchable slice of the postgres
//! store. This makes it disposable and rebuildable!

use heart::PackageId;
use sqlx::Row;
use tantivy::{Index, IndexReader, schema::Field};

use crate::{
	GlobalPackage,
	error::SearchError,
	schema::{codec, queries},
};

/// The last postgres position a replica has folded into its local index — the
/// watermark it resumes syncing from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SyncWatermark {
	/// The postgres logical sequence (e.g. an `updated_at` cursor or txid) last
	/// consumed.
	pub position: i64,
}

/// Filename for the durable sync watermark, next to the index directory root.
const WATERMARK_FILE: &str = "sync_watermark.json";

/// A replica-local tantivy index over the searchable package projection.
pub struct PackageIndex {
	index:     Index,
	reader:    IndexReader,
	watermark: SyncWatermark,
	/// Directory the index (and watermark file) live in.
	dir:       std::path::PathBuf,
}

/// The resolved schema fields, looked up per operation so writer and reader can
/// never disagree on names.
struct Fields {
	package_id:  Field,
	name:        Field,
	description: Field,
	keywords:    Field,
	ecosystem:   Field,
	record:      Field,
}

/// The indexing heap handed to the tantivy writer per sync (tantivy's floor is
/// 15 MB; this leaves comfortable headroom for large batches).
const WRITER_HEAP_BYTES: usize = 50 << 20;

/// How many changed rows one sync poll folds in.
const SYNC_BATCH: u64 = 1024;

impl PackageIndex {
	/// Open (or create) the replica-local index at `path`.
	///
	/// Restores a previously persisted sync watermark when present so restarts
	/// do not re-fold all of postgres history. Rebuild-from-postgres remains
	/// the recovery path if the watermark file is missing or corrupt.
	pub fn open(path: &std::path::Path) -> Result<Self, SearchError> {
		std::fs::create_dir_all(path)
			.map_err(|e| SearchError::Tantivy(tantivy::TantivyError::from(e)))?;
		let directory = tantivy::directory::MmapDirectory::open(path)
			.map_err(|e| SearchError::Tantivy(e.into()))?;
		let index =
			Index::open_or_create(directory, Self::schema()).map_err(SearchError::Tantivy)?;
		let reader = index.reader().map_err(SearchError::Tantivy)?;
		let mut watermark = load_watermark(path);
		// Orphan watermark + empty index (dir recreated, file left behind) would
		// skip history on resume — reset to zero when there is nothing folded.
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
			"replica-local package index opened"
		);
		Ok(Self {
			index,
			reader,
			watermark,
			dir: path.to_path_buf(),
		})
	}

	/// The tantivy schema fields the package projection indexes. Defined once so
	/// writer and reader agree.
	pub fn schema() -> tantivy::schema::Schema {
		use tantivy::schema::{STORED, STRING, TEXT};
		let mut builder = tantivy::schema::Schema::builder();
		// Raw-indexed for exact term lookups (delete-on-upsert, hydrate).
		builder.add_text_field("package_id", STRING | STORED);
		// The searchable projection (description/keywords await richer package
		// metadata; declared now so the schema never needs a breaking rebuild).
		builder.add_text_field("name", TEXT);
		builder.add_text_field("description", TEXT);
		builder.add_text_field("keywords", TEXT);
		builder.add_text_field("ecosystem", STRING | STORED);
		// The full serialized `GlobalPackage`, stored (never indexed) so hydrate
		// needs no postgres round-trip.
		builder.add_text_field("record", STORED);
		builder.build()
	}

	/// Resolve every schema field by name (total: [`PackageIndex::schema`]
	/// always defines them).
	fn fields(&self) -> Result<Fields, SearchError> {
		let schema = self.index.schema();
		let field = |name| schema.get_field(name).map_err(SearchError::Tantivy);
		Ok(Fields {
			package_id: field("package_id")?,
			name: field("name")?,
			description: field("description")?,
			keywords: field("keywords")?,
			ecosystem: field("ecosystem")?,
			record: field("record")?,
		})
	}

	/// Fold a batch of package records into the local index at `position`,
	/// upserting each by its deterministic id and advancing the watermark. The
	/// unit [`PackageIndex::sync_from`] applies to postgres output; exposed so a
	/// rebuild can replay records from any source (e.g. blob manifests).
	/// `// tantivy commit + fsync run on spawn_blocking`.
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
			// Upsert: delete any prior generation of this package, then re-add.
			writer.delete_term(tantivy::Term::from_field_text(fields.package_id, &token));

			let json = serde_json::to_string(record).map_err(|e| SearchError::JsonDecode { domain: "GlobalPackage", source: e })?;
			let coordinates = &record.package.coordinates;
			let mut document = tantivy::TantivyDocument::default();
			document.add_text(fields.package_id, &token);
			document.add_text(fields.name, coordinates.name.canonical());
			document.add_text(fields.name, coordinates.name.original());
			document.add_text(fields.ecosystem, coordinates.ecosystem().as_token());
			// Derived keywords (when rich metadata has been extracted) feed the
			// searchable `keywords` field; absent facets simply index no keywords.
			if let Some(facets) = &record.facets {
				document.add_text(fields.keywords, facets.keyword_text());
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

	/// Poll postgres from the current watermark and fold new/changed packages
	/// into the local index, advancing the watermark. Idempotent and resumable.
	/// `// tantivy commit + fsync run on spawn_blocking`.
	#[tracing::instrument(skip_all, fields(from = self.watermark.position))]
	pub async fn sync_from(&mut self, pool: &sqlx::PgPool) -> Result<SyncWatermark, SearchError> {
		let after = chrono::DateTime::from_timestamp_micros(self.watermark.position)
			.unwrap_or(chrono::DateTime::UNIX_EPOCH);
		let (sql, vals) = queries::search::changed_since(after, SYNC_BATCH);
		let rows =
			sqlx::query_with(&sql, vals).fetch_all(pool).await.map_err(SearchError::Source)?;

		let mut head = self.watermark.position;
		let mut records = Vec::with_capacity(rows.len());
		for row in &rows {
			let (record, updated_micros) = row_to_record(row)?;
			head = head.max(updated_micros);
			records.push(record);
		}
		self.absorb(records.iter(), head)
	}

	/// Execute a text query, returning the matching package ids with their raw
	/// tantivy scores (ranking + access-filter happen a layer up). `// tantivy
	/// search runs on spawn_blocking`.
	pub fn query(
		&self,
		text: &str,
		limit: usize,
	) -> Result<Vec<(PackageId, f32)>, SearchError> {
		use tantivy::collector::TopDocs;

		let fields = self.fields()?;
		let searcher = self.reader.searcher();
		let parser = tantivy::query::QueryParser::for_index(
			&self.index,
			vec![fields.name, fields.description, fields.keywords],
		);
		let query = parser.parse_query(text).map_err(|e| {
			SearchError::TantivyQueryParse(tantivy::TantivyError::InvalidArgument(e.to_string()))
		})?;
		let top = searcher
			.search(&query, &TopDocs::with_limit(limit.max(1)))
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

	/// Hydrate matched package ids into full [`GlobalPackage`] records from the
	/// stored projection (no postgres round-trip). Ids the replica has not yet
	/// heard about are skipped, not fatal — the replica is eventually
	/// consistent by design.
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
			records.push(serde_json::from_str(&json).map_err(|e| SearchError::JsonDecode { domain: "GlobalPackage", source: e })?);
		}
		Ok(records)
	}

	/// The current sync watermark.
	pub fn watermark(&self) -> SyncWatermark { self.watermark }
}

/// Read `sync_watermark.json` next to the index, or `{ position: 0 }` on miss/corrupt.
///
/// `NotFound` is the common cold-start path (silent zero). Other I/O errors and
/// corrupt JSON log a warning and still resume from zero — rebuild-from-postgres
/// remains the recovery story.
fn load_watermark(dir: &std::path::Path) -> SyncWatermark {
	let path = dir.join(WATERMARK_FILE);
	match std::fs::read(&path) {
		Ok(bytes) => match serde_json::from_slice::<SyncWatermark>(&bytes) {
			Ok(wm) => wm,
			Err(e) => {
				tracing::warn!(
					path = %path.display(),
					error = %e,
					"corrupt tantivy watermark; resuming from zero"
				);
				SyncWatermark { position: 0 }
			},
		},
		Err(e) if e.kind() == std::io::ErrorKind::NotFound => SyncWatermark { position: 0 },
		Err(e) => {
			tracing::warn!(
				path = %path.display(),
				error = %e,
				"failed to read tantivy watermark; resuming from zero"
			);
			SyncWatermark { position: 0 }
		},
	}
}

/// Best-effort durable watermark write (tmp + rename).
fn persist_watermark(dir: &std::path::Path, watermark: SyncWatermark) {
	let path = dir.join(WATERMARK_FILE);
	let Ok(bytes) = serde_json::to_vec(&watermark) else {
		return;
	};
	let tmp = path.with_extension("json.tmp");
	if let Err(e) = std::fs::write(&tmp, &bytes) {
		tracing::warn!(path = %tmp.display(), error = %e, "failed to write tantivy watermark");
		return;
	}
	if let Err(e) = std::fs::rename(&tmp, &path) {
		tracing::warn!(path = %path.display(), error = %e, "failed to persist tantivy watermark");
	}
}

/// The stored text value of `field`, or an internal error if the document
/// violates the schema contract (impossible for documents this module wrote).
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

// internal() removed; use specific SearchError variants (TantivyInternal, StoredIdNotUuid, JsonDecode) carrying concrete sources.

/// A codec failure while decoding a polled row — a corrupt-row invariant break,
/// surfaced on the source (postgres) channel. Use Codec variant.
fn codec_to_search(e: codec::CodecError) -> SearchError {
	SearchError::Codec(e)
}

/// Reassemble one `(GlobalPackage, updated_at micros)` from a
/// [`queries::search::changed_since`] row. Column order: id, language,
/// origin_token, name_canonical, name_original, version_canonical, toolchain,
/// updated_at, then the (left-joined, possibly absent) lifecycle columns
/// state, phase, content_hash, needed, failure, facets.
fn row_to_record(row: &sqlx::postgres::PgRow) -> Result<(GlobalPackage, i64), SearchError> {
	let id: uuid::Uuid = row.try_get(0).map_err(SearchError::Source)?;
	let language: String = row.try_get(1).map_err(SearchError::Source)?;
	let origin_token: String = row.try_get(2).map_err(SearchError::Source)?;
	let name_original: String = row.try_get(4).map_err(SearchError::Source)?;
	let version_canonical: String = row.try_get(5).map_err(SearchError::Source)?;
	let toolchain_json: serde_json::Value = row.try_get(6).map_err(SearchError::Source)?;
	let updated_at: chrono::DateTime<chrono::Utc> = row.try_get(7).map_err(SearchError::Source)?;
	let state: Option<String> = row.try_get(8).map_err(SearchError::Source)?;
	let phase: Option<String> = row.try_get(9).map_err(SearchError::Source)?;
	let content_hash: Option<Vec<u8>> = row.try_get(10).map_err(SearchError::Source)?;
	let needed: Option<bool> = row.try_get(11).map_err(SearchError::Source)?;
	let failure: Option<serde_json::Value> = row.try_get(12).map_err(SearchError::Source)?;
	let facets_json: Option<serde_json::Value> = row.try_get(13).map_err(SearchError::Source)?;

	let coordinates = codec::coordinates_from_columns(
		&language,
		&origin_token,
		&name_original,
		&version_canonical,
	)
	.map_err(codec_to_search)?;
	let toolchain = codec::toolchain_from_json(&toolchain_json).map_err(codec_to_search)?;
	let state = match state {
		// No lifecycle row yet: the package exists but indexing never started.
		None => heart::ResolutionState::Unindexed { needed: false },
		Some(token) => codec::state_from_columns(
			&token,
			phase.as_deref(),
			content_hash.as_deref(),
			needed.unwrap_or(false),
			failure.as_ref(),
		)
		.map_err(codec_to_search)?,
	};

	let facets = codec::facets_from_json(facets_json.as_ref()).map_err(codec_to_search)?;

	let package = crate::Package { coordinates, toolchain };
	// The postgres sync path now carries the derived facets from `ps.facets`
	// (nullable jsonb, mirroring `ps.failure`): keywords + quality flow straight
	// into the replica index and the ranking fusion. `None` when metadata was
	// never extracted for this generation.
	let record =
		GlobalPackage { id: codec::package_id_from_uuid(id), package, state, facets };
	Ok((record, updated_at.timestamp_micros()))
}
