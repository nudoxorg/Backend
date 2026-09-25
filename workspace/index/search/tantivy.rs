//! Replica-local tantivy index over the searchable package projection.
//!
//! Schema v4 (SCHEMA_VERSION = 4):
//! - `package_id` STRING|STORED — upsert/delete key
//! - `name_exact` STRING — search_surface lowercased + canonical lowercased
//!   + original lowercased (multiple values per doc)
//! - `name_tokens` TEXT(ident) — search_surface + original,
//!   identifier-tokenized
//! - `name_ns` TEXT(ident) — namespace segments space-joined
//! - `description` TEXT — from SearchFacets.description (S2)
//! - `keywords` TEXT — from SearchFacets.keyword_text() plus index-time name
//!   separator parts ([`super::ranking::enrich`]; no separate `extra` field)
//! - `ecosystem` STRING|STORED — language token (Must filter, Q4)
//! - `record` STORED — full serialized GlobalPackage (hydrate)
//! - `deps` STRING — one value per facets.dependencies slug (multi-valued)
//! - `license` STRING — facets.license as a single lowercase term, when present
//! - `repo` STRING — facets.repo_slug, when present
//! - `quality_ppm` FAST u64 — facets.quality_ppm (0 if unknown)
//! - `downloads` FAST u64 — facets.downloads (0 if None)
//! - `popularity_pct_ppm` FAST u64 — eco CDF × 1e6 (0 if None / not yet filled)
//!
//! FAST ranking fields are for future collectors and debug; the live rank path
//! still hydrates the full `record` JSON. Query matching is unchanged from v3.
//!
//! The IdentifierTokenizer from registry/runtime/text/tokenizer.rs is
//! re-registered on this index after every open (same mechanism as TextIndex).

use super::structured::StructuredQuery;
use crate::{
    GlobalPackage, ecosystem::PackageNameExt as _, error::SearchError, runtime::text::tokenizer,
    schema::codec,
};
use heart::PackageId;
use tantivy::{Index, IndexReader, schema::Field};

/// Schema version written to `schema_version` next to the index dir.
/// Mismatch on open → wipe contents + reset watermark to 0.
const SCHEMA_VERSION: u32 = 4;

/// Filename for the durable sync watermark.
const WATERMARK_FILE: &str = "sync_watermark.json";

/// Filename for the schema version marker.
const SCHEMA_VERSION_FILE: &str = "schema_version";

/// Tantivy writer heap per sync batch.
const WRITER_HEAP_BYTES: usize = 50 << 20;

/// Postgres batch size per sync poll.
const SYNC_BATCH: u64 = 1024;

/// All query boost values in one place so tuning is one-line. (§8.3)
struct PackageQueryBoosts;

impl PackageQueryBoosts {
    const EXACT: f32 = 4.0;
    const NAME_TOKENS: f32 = 2.0;
    const NAME_NS: f32 = 1.5;
    const DESCRIPTION: f32 = 1.0;
    const KEYWORDS: f32 = 1.2;
    const FUZZY: f32 = 0.3;
    /// Boost applied to synonym-expanded keyword matches.
    const EXPANDED: f32 = 0.5;
}

/// The last postgres position a replica has folded into its local index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SyncWatermark {
    pub position: i64,
}

/// Ranking signals stored as FAST u64 columns (schema v4).
///
/// Read via [`PackageIndex::fast_ranking_signals`] for collectors / debug /
/// round-trip tests. Live ranking still uses hydrated facets.
///
/// Design notes (sensible defaults + uniform scale):
/// - `quality_ppm` and `popularity_pct_ppm` share a 0..=1_000_000 domain so
///   future scoring formulas can treat them as interchangeable fractions
///   without extra normalisation.
/// - `downloads` stays a raw count; collectors that need log-scale or
///   percentile transforms do that at read time (FAST columns are cheap to
///   re-read).
/// - Missing facets always write 0 so every document has the three columns;
///   collectors never have to special-case “field absent”.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FastRankingSignals {
    /// Quality in parts-per-million (`0..=1_000_000`); `0` if facets unknown.
    pub quality_ppm: u64,
    /// Monthly downloads; `0` when facets omit downloads (`None`).
    pub downloads: u64,
    /// Ecosystem popularity percentile as ppm (`0..=1_000_000`); `0` if unset.
    pub popularity_pct_ppm: u64,
}

/// Schema field handles — resolved once at open so writer and reader always
/// agree.
#[derive(Clone, Copy)]
struct Fields {
    package_id: Field,
    name_exact: Field,
    name_tokens: Field,
    name_ns: Field,
    description: Field,
    keywords: Field,
    ecosystem: Field,
    record: Field,
    /// One value per dependency slug (multi-valued STRING).
    deps: Field,
    /// Lowercased license expression, when present.
    license: Field,
    /// `host/owner/repo` slug, when present.
    repo: Field,
    /// v4 FAST ranking columns.
    quality_ppm: Field,
    downloads: Field,
    popularity_pct_ppm: Field,
}

/// A replica-local tantivy index over the searchable package projection.
pub struct PackageIndex {
    index: Index,
    reader: IndexReader,
    fields: Fields,
    watermark: SyncWatermark,
    dir: std::path::PathBuf,
}

impl PackageIndex {
    /// Open (or create) the replica-local index at `path`.
    ///
    /// On schema version mismatch or missing marker with non-empty index,
    /// the index directory contents are wiped and the watermark is reset to
    /// zero so a full resync rebuilds the index under the new schema.
    pub fn open(path: &std::path::Path) -> Result<Self, SearchError> {
        std::fs::create_dir_all(path)
            .map_err(|error| SearchError::Tantivy(tantivy::TantivyError::from(error)))?;

        Self::maybe_wipe_for_schema_change(path);

        let directory = tantivy::directory::MmapDirectory::open(path)
            .map_err(|error| SearchError::Tantivy(error.into()))?;
        let index =
            Index::open_or_create(directory, Self::schema()).map_err(SearchError::Tantivy)?;

        // Register the identifier tokenizer (same mechanism as
        // TextIndex::open_or_create).
        tokenizer::register(&index);

        let reader = index.reader().map_err(SearchError::Tantivy)?;
        let fields = resolve_fields(&index)?;

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

        Ok(Self {
            index,
            reader,
            fields,
            watermark,
            dir: path.to_path_buf(),
        })
    }

    /// Wipe the index directory contents and reset the watermark when the
    /// on-disk schema version does not match `SCHEMA_VERSION`.
    fn maybe_wipe_for_schema_change(path: &std::path::Path) {
        let marker_path = path.join(SCHEMA_VERSION_FILE);
        let on_disk_version = std::fs::read_to_string(&marker_path)
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok());

        let needs_wipe = on_disk_version != Some(SCHEMA_VERSION) && directory_has_content(path);

        if needs_wipe {
            tracing::warn!(
                path = %path.display(),
                on_disk = ?on_disk_version,
                current = SCHEMA_VERSION,
                "schema version mismatch; wiping index for resync"
            );
            wipe_directory_contents(path);
            let _ = std::fs::remove_file(path.join(WATERMARK_FILE));
        }

        // Always write the current schema version marker (fresh or post-wipe).
        if std::fs::write(&marker_path, SCHEMA_VERSION.to_string()).is_err() {
            tracing::warn!(
                path = %marker_path.display(),
                "failed to write schema_version marker"
            );
        }
    }

    /// Schema v4: all fields (v3 text/facet filters + FAST ranking columns).
    pub fn schema() -> tantivy::schema::Schema {
        use tantivy::schema::{
            FAST, IndexRecordOption, STORED, STRING, TEXT, TextFieldIndexing, TextOptions,
        };

        let mut builder = tantivy::schema::Schema::builder();

        builder.add_text_field("package_id", STRING | STORED);

        // Exact-match field: stores search_surface, canonical, and original
        // (all lowercased) as separate values for O(1) TermQuery lookup.
        builder.add_text_field("name_exact", STRING);

        // Subtoken fields: identifier-tokenized so `mux` finds `gorilla/mux`.
        let identifier_options = TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer(tokenizer::IDENT_TOKENIZER)
                .set_index_option(IndexRecordOption::WithFreqsAndPositions),
        );
        builder.add_text_field("name_tokens", identifier_options.clone());
        builder.add_text_field("name_ns", identifier_options);

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
        // Always written (default 0) so every document has the three columns.
        builder.add_u64_field("quality_ppm", FAST);
        builder.add_u64_field("downloads", FAST);
        builder.add_u64_field("popularity_pct_ppm", FAST);

        builder.build()
    }

    /// Fold a batch of package records into the local index at `position`.
    pub fn absorb<'record>(
        &mut self,
        records: impl IntoIterator<Item = &'record GlobalPackage>,
        position: i64,
    ) -> Result<SyncWatermark, SearchError> {
        let mut writer = self
            .index
            .writer(WRITER_HEAP_BYTES)
            .map_err(SearchError::Tantivy)?;

        let mut folded = 0usize;
        for record in records {
            let package_token = record.id.to_string();
            writer.delete_term(tantivy::Term::from_field_text(
                self.fields.package_id,
                &package_token,
            ));

            let document = build_document(&self.fields, record)?;
            writer
                .add_document(document)
                .map_err(SearchError::Tantivy)?;
            folded += 1;
        }

        writer.commit().map_err(SearchError::Tantivy)?;
        self.reader.reload().map_err(SearchError::Tantivy)?;
        self.watermark = SyncWatermark { position };
        persist_watermark(&self.dir, self.watermark);

        tracing::debug!(
            folded,
            position,
            "package records absorbed into replica index"
        );
        Ok(self.watermark)
    }

    /// Poll the catalog outbox (Text sink) from the current watermark and fold
    /// new/changed packages. The watermark position is the outbox `seq` (it was
    /// a postgres timestamp before the catalog port; both are monotonic i64s,
    /// so persisted watermarks stay decodable — a stale timestamp value simply
    /// reads as "far ahead" and the next full rebuild resets it).
    #[tracing::instrument(skip_all, fields(from = self.watermark.position))]
    pub async fn sync_from<Engine: crate::engine::VersioningEngine + Send + Sync>(
        &mut self,
        global: &crate::catalog::GlobalStore<Engine>,
        outbox: &crate::coordination::Outbox<Engine>,
    ) -> Result<SyncWatermark, SearchError> {
        let entries = outbox
            .read_since(
                crate::coordination::SinkKind::Text,
                crate::coordination::OutboxSeq(self.watermark.position),
                SYNC_BATCH as usize,
            )
            .await
            .map_err(|error| SearchError::OutboxRead {
                detail: error.to_string(),
            })?;

        let mut head = self.watermark.position;
        let mut records = Vec::with_capacity(entries.len());

        for entry in &entries {
            head = head.max(entry.id.0);
            match global.get(entry.package).await {
                Ok(record) => records.push(record),
                // A version row can trail its outbox intent (or be tombstoned);
                // skip and let the next intent re-deliver it.
                Err(crate::error::IndexError::NotFound { .. }) => {}
                Err(error) => {
                    return Err(SearchError::OutboxRead {
                        detail: error.to_string(),
                    });
                }
            }
        }

        self.absorb(records.iter(), head)
    }

    /// Execute a raw-text query (backward-compat for existing tests/callers).
    /// Delegates to [`Self::query_structured`] via `StructuredQuery::parse`.
    pub fn query(&self, text: &str, limit: usize) -> Result<Vec<(PackageId, f32)>, SearchError> {
        let structured = StructuredQuery::parse(text, None);
        Ok(self
            .query_structured(&structured, limit)?
            .into_iter()
            .map(|(package, score)| (package.id, score))
            .collect())
    }

    /// Execute a structured query, returning each hit's stored package with its
    /// score.
    ///
    /// The search already opens the stored document to read the package id. The
    /// record JSON is in that same document, so the caller does not search
    /// again.
    ///
    /// Builds a hand-written BooleanQuery tree (§8.3). No tantivy QueryParser
    /// is used — terms are raw, no grammar escaping needed (Q1 fix).
    pub fn query_structured(
        &self,
        structured: &StructuredQuery,
        limit: usize,
    ) -> Result<Vec<(GlobalPackage, f32)>, SearchError> {
        use tantivy::{
            Term,
            collector::TopDocs,
            query::{BooleanQuery, BoostQuery, FuzzyTermQuery, Occur, PhraseQuery, TermQuery},
            schema::IndexRecordOption,
        };

        let searcher = self.reader.searcher();
        let fields = &self.fields;

        // Produce subtokens from the identifier tokenizer (same split as index time).
        // Drop tokens of length < 2 from Must-conjunctions (T2).
        let terms_lower = structured.terms.to_ascii_lowercase();
        let filtered_subtokens: Vec<String> = tokenizer::subtokens(&structured.terms)
            .into_iter()
            .filter(|token| token.len() >= 2)
            .collect();

        let mut outer_clauses: Vec<(Occur, Box<dyn tantivy::query::Query>)> = Vec::new();

        // AllQuery fires when there are no free terms — dep:/license:/namespace
        // filters are separate Must clauses and compose naturally with AllQuery.
        if structured.terms.is_empty() {
            outer_clauses.push((Occur::Must, Box::new(tantivy::query::AllQuery)));
        } else {
            let mut should_clauses: Vec<(Occur, Box<dyn tantivy::query::Query>)> = Vec::new();

            // Tier 1: exact match on name_exact (full terms lowercased), boost 4.0.
            {
                let exact_term = Term::from_field_text(fields.name_exact, &terms_lower);
                let exact_query = TermQuery::new(exact_term, IndexRecordOption::Basic);
                should_clauses.push((
                    Occur::Should,
                    Box::new(BoostQuery::new(
                        Box::new(exact_query),
                        PackageQueryBoosts::EXACT,
                    )),
                ));
            }

            // Tiers 2–5: Must-conjunction of subtokens on the four text fields.
            // Identical shape → one loop; only field + boost differ.
            if !filtered_subtokens.is_empty() {
                let text_tiers = [
                    (fields.name_tokens, PackageQueryBoosts::NAME_TOKENS),
                    (fields.name_ns, PackageQueryBoosts::NAME_NS),
                    (fields.description, PackageQueryBoosts::DESCRIPTION),
                    (fields.keywords, PackageQueryBoosts::KEYWORDS),
                ];
                for (field, boost) in text_tiers {
                    let conjunction = must_conjunction_of_terms(field, &filtered_subtokens);
                    should_clauses.push((
                        Occur::Should,
                        Box::new(BoostQuery::new(Box::new(conjunction), boost)),
                    ));
                }
            }

            // Tier 6: FuzzyTermQuery — only for single tokens of len 4..=12 (T1).
            let is_single_token = structured.terms.split_whitespace().count() == 1;
            if is_single_token && (4..=12).contains(&terms_lower.len()) {
                // Whole-name typos match against name_exact (raw STRING terms);
                // name_tokens only holds sub-words, so the full query term would
                // never fuzzy-match there. A second clause on name_tokens catches
                // a typo in one sub-word of a multi-part name (`gorila` → `mux`'s
                // `gorilla`). Exact distance-1, transpositions allowed — NOT the
                // prefix variant, which would silently widen matching.
                for field in [fields.name_exact, fields.name_tokens] {
                    let term = Term::from_field_text(field, &terms_lower);
                    let fuzzy = FuzzyTermQuery::new(term, 1, true);
                    should_clauses.push((
                        Occur::Should,
                        Box::new(BoostQuery::new(Box::new(fuzzy), PackageQueryBoosts::FUZZY)),
                    ));
                }
            }

            // Tier 7: synonym expansions — Should-disjunction on keywords, boost 0.5.
            // Only fires when expand_synonyms has been called.
            if !structured.expanded_terms.is_empty() {
                let expansion_clauses: Vec<(Occur, Box<dyn tantivy::query::Query>)> = structured
                    .expanded_terms
                    .iter()
                    .map(|term_text| {
                        let term = Term::from_field_text(fields.keywords, term_text);
                        let term_query: Box<dyn tantivy::query::Query> =
                            Box::new(TermQuery::new(term, IndexRecordOption::WithFreqs));
                        (Occur::Should, term_query)
                    })
                    .collect();
                let expansion_block = BooleanQuery::new(expansion_clauses);
                should_clauses.push((
                    Occur::Should,
                    Box::new(BoostQuery::new(
                        Box::new(expansion_block),
                        PackageQueryBoosts::EXPANDED,
                    )),
                ));
            }

            // Wrap the Should tiers so at least one must match.
            let name_block = BooleanQuery::new(should_clauses);
            outer_clauses.push((Occur::Must, Box::new(name_block)));
        }

        // Q4: Must TermQuery(ecosystem) when known — index-pruned, not post-filter.
        if let Some(ecosystem) = structured.ecosystem {
            let ecosystem_term = Term::from_field_text(fields.ecosystem, ecosystem.as_token());
            outer_clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(ecosystem_term, IndexRecordOption::Basic)),
            ));
        }

        // Namespace constraint: Must subtoken-conjunction on name_ns.
        if let Some(namespace) = &structured.namespace {
            let namespace_subtokens: Vec<String> = tokenizer::subtokens(namespace)
                .into_iter()
                .filter(|token| token.len() >= 2)
                .collect();
            if !namespace_subtokens.is_empty() {
                outer_clauses.push((
                    Occur::Must,
                    Box::new(must_conjunction_of_terms(
                        fields.name_ns,
                        &namespace_subtokens,
                    )),
                ));
            }
        }

        // dep: filters — Must TermQuery on the `deps` field for each value.
        for dependency_name in &structured.deps {
            let term = Term::from_field_text(fields.deps, dependency_name);
            outer_clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
            ));
        }

        // license: filter — Must TermQuery on `license` when present.
        if let Some(license) = &structured.license {
            let term = Term::from_field_text(fields.license, license);
            outer_clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
            ));
        }

        // Phrase filters: for each phrase with ≥ 2 subtokens, build a PhraseQuery
        // on `description` AND `keywords` (both TEXT = positions indexed). The two
        // are wrapped as a Should-pair inside one Must BooleanQuery so the phrase
        // must appear in at least one prose field.
        for phrase in &structured.phrases {
            let phrase_subtokens = tokenizer::subtokens(phrase);
            if phrase_subtokens.len() < 2 {
                // < 2 usable subtokens — words are already in terms; skip.
                continue;
            }

            let make_phrase_query = |field: Field| -> Box<dyn tantivy::query::Query> {
                let offset_terms: Vec<(usize, Term)> = phrase_subtokens
                    .iter()
                    .enumerate()
                    .map(|(position, token)| (position, Term::from_field_text(field, token)))
                    .collect();
                Box::new(PhraseQuery::new_with_offset(offset_terms))
            };

            let phrase_clauses = vec![
                (Occur::Should, make_phrase_query(fields.description)),
                (Occur::Should, make_phrase_query(fields.keywords)),
            ];
            outer_clauses.push((Occur::Must, Box::new(BooleanQuery::new(phrase_clauses))));
        }

        let top_query = BooleanQuery::new(outer_clauses);
        let top_docs = searcher
            .search(&top_query, &TopDocs::with_limit(limit.max(1)))
            .map_err(SearchError::Tantivy)?;

        top_docs
            .into_iter()
            .map(|(score, address)| {
                let document: tantivy::TantivyDocument =
                    searcher.doc(address).map_err(SearchError::Tantivy)?;
                let identifier = stored_text(&document, fields.package_id)?
                    .parse::<uuid::Uuid>()
                    .map_err(|_| SearchError::StoredIdNotUuid)?;
                let json = stored_text(&document, fields.record)?;
                let mut package: GlobalPackage =
                    serde_json::from_str(&json).map_err(|error| SearchError::JsonDecode {
                        domain: "GlobalPackage",
                        source: error,
                    })?;
                // The stored id is the term the index was built under. Ranking
                // looks the BM25 score up by `package.id`, so the decoded
                // record must carry that same id.
                package.id = codec::package_id_from_uuid(identifier);
                Ok((package, score))
            })
            .collect()
    }

    /// Hydrate matched package ids from the stored projection.
    pub async fn hydrate(
        &self,
        identifiers: &[PackageId],
    ) -> Result<Vec<GlobalPackage>, SearchError> {
        use tantivy::{Term, collector::TopDocs, query::TermQuery, schema::IndexRecordOption};

        let searcher = self.reader.searcher();
        let fields = &self.fields;
        let mut records = Vec::with_capacity(identifiers.len());

        for identifier in identifiers {
            let term = Term::from_field_text(fields.package_id, &identifier.to_string());
            let query = TermQuery::new(term, IndexRecordOption::Basic);
            let top_docs = searcher
                .search(&query, &TopDocs::with_limit(1))
                .map_err(SearchError::Tantivy)?;

            let Some((_, address)) = top_docs.into_iter().next() else {
                tracing::debug!(
                    package = %identifier,
                    "hydrate miss against replica index"
                );
                continue;
            };

            let document: tantivy::TantivyDocument =
                searcher.doc(address).map_err(SearchError::Tantivy)?;
            let json = stored_text(&document, fields.record)?;
            records.push(
                serde_json::from_str(&json).map_err(|error| SearchError::JsonDecode {
                    domain: "GlobalPackage",
                    source: error,
                })?,
            );
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
        identifier: PackageId,
    ) -> Result<Option<FastRankingSignals>, SearchError> {
        use tantivy::{Term, collector::TopDocs, query::TermQuery, schema::IndexRecordOption};

        let searcher = self.reader.searcher();
        let fields = &self.fields;

        let term = Term::from_field_text(fields.package_id, &identifier.to_string());
        let query = TermQuery::new(term, IndexRecordOption::Basic);
        let top_docs = searcher
            .search(&query, &TopDocs::with_limit(1))
            .map_err(SearchError::Tantivy)?;

        let Some((_, address)) = top_docs.into_iter().next() else {
            return Ok(None);
        };

        let segment_reader = searcher.segment_reader(address.segment_ord);
        let fast_fields = segment_reader.fast_fields();

        let quality = fast_fields
            .u64("quality_ppm")
            .map_err(SearchError::Tantivy)?;
        let downloads = fast_fields.u64("downloads").map_err(SearchError::Tantivy)?;
        let popularity = fast_fields
            .u64("popularity_pct_ppm")
            .map_err(SearchError::Tantivy)?;

        Ok(Some(FastRankingSignals {
            quality_ppm: quality.first(address.doc_id).unwrap_or(0),
            downloads: downloads.first(address.doc_id).unwrap_or(0),
            popularity_pct_ppm: popularity.first(address.doc_id).unwrap_or(0),
        }))
    }

    /// Snapshot of index health for ops dashboards (doc count, watermark,
    /// schema).
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

    pub fn watermark(&self) -> SyncWatermark {
        self.watermark
    }

    /// Remove a package's document from the replica-local index by its id.
    ///
    /// Issues a `delete_term` on the `package_id` field and commits immediately
    /// so the removal is visible to the next searcher reload. Analogous to the
    /// `absorb`'s delete-before-add but without the subsequent add.
    ///
    /// Called by the outbox consumer on a `Delete` intent — the mirror
    /// tombstone path. Blob / CAS data is retained; only the search
    /// projection is removed.
    pub fn remove(&mut self, package: PackageId) -> Result<(), SearchError> {
        let package_token = package.to_string();
        let mut writer: tantivy::IndexWriter<tantivy::TantivyDocument> = self
            .index
            .writer(WRITER_HEAP_BYTES)
            .map_err(SearchError::Tantivy)?;

        writer.delete_term(tantivy::Term::from_field_text(
            self.fields.package_id,
            &package_token,
        ));
        writer.commit().map_err(SearchError::Tantivy)?;
        self.reader.reload().map_err(SearchError::Tantivy)?;

        tracing::debug!(%package, "package removed from replica search index");
        Ok(())
    }
}

// ─── private helpers ────────────────────────────────────────────────────────

fn resolve_fields(index: &Index) -> Result<Fields, SearchError> {
    let schema = index.schema();
    let field = |name: &str| schema.get_field(name).map_err(SearchError::Tantivy);
    Ok(Fields {
        package_id: field("package_id")?,
        name_exact: field("name_exact")?,
        name_tokens: field("name_tokens")?,
        name_ns: field("name_ns")?,
        description: field("description")?,
        keywords: field("keywords")?,
        ecosystem: field("ecosystem")?,
        record: field("record")?,
        deps: field("deps")?,
        license: field("license")?,
        repo: field("repo")?,
        quality_ppm: field("quality_ppm")?,
        downloads: field("downloads")?,
        popularity_pct_ppm: field("popularity_pct_ppm")?,
    })
}

fn directory_has_content(path: &std::path::Path) -> bool {
    std::fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_some())
}

fn wipe_directory_contents(path: &std::path::Path) {
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Build a Must-conjunction of TermQueries (WithFreqs) over the given tokens.
fn must_conjunction_of_terms(field: Field, tokens: &[String]) -> tantivy::query::BooleanQuery {
    use tantivy::{
        Term,
        query::{BooleanQuery, Occur, TermQuery},
        schema::IndexRecordOption,
    };

    let clauses: Vec<(Occur, Box<dyn tantivy::query::Query>)> = tokens
        .iter()
        .map(|token| {
            let term = Term::from_field_text(field, token);
            let term_query: Box<dyn tantivy::query::Query> =
                Box::new(TermQuery::new(term, IndexRecordOption::WithFreqs));
            (Occur::Must, term_query)
        })
        .collect();
    BooleanQuery::new(clauses)
}

/// Extract the three FAST ranking columns from optional facets.
///
/// Always returns concrete values (0 when facets are absent) so every document
/// written by `absorb` has the columns present and collectors never see
/// “field missing”.
fn ranking_signals_from_facets(facets: Option<&crate::metadata::SearchFacets>) -> (u64, u64, u64) {
    facets.map_or((0, 0, 0), |facets| {
        let quality_ppm = u64::from(facets.quality_ppm);
        let downloads = facets.downloads.unwrap_or(0);
        // popularity_pct is stored as 0..=10_000 (basis points of a percent).
        // Scale by 100 → 0..=1_000_000 so it shares the same domain as quality_ppm.
        let popularity_pct_ppm = facets
            .popularity_pct
            .map_or(0, |value| u64::from(value.min(10_000)) * 100);
        (quality_ppm, downloads, popularity_pct_ppm)
    })
}

/// Construct a complete TantivyDocument for a single GlobalPackage.
fn build_document(
    fields: &Fields,
    record: &GlobalPackage,
) -> Result<tantivy::TantivyDocument, SearchError> {
    let package_token = record.id.to_string();
    let json = serde_json::to_string(record).map_err(|error| SearchError::JsonDecode {
        domain: "GlobalPackage",
        source: error,
    })?;

    let coordinates = &record.package.coordinates;
    let name = &coordinates.name;
    let structured = name.structured();

    let canonical_lower = name.canonical().to_ascii_lowercase();
    let original_lower = name.original().to_ascii_lowercase();
    // search_surface: namespace + name WITHOUT authority (R4 — `github`/
    // `com` never become tokens).
    let search_surface = structured.search_surface();
    let search_surface_lower = search_surface.to_ascii_lowercase();
    let namespace_text = structured.namespace.join(" ");

    let mut document = tantivy::TantivyDocument::default();
    document.add_text(fields.package_id, &package_token);

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
    let ecosystem = coordinates.ecosystem();
    let base_keywords = record
        .facets
        .as_ref()
        .map(crate::metadata::SearchFacets::keyword_text)
        .unwrap_or_default();
    let enrichment = super::ranking::enrich::enrich_package_text(
        &search_surface_lower,
        &base_keywords,
        ecosystem,
    );
    let keywords_text = super::ranking::enrich::merge_keywords(&base_keywords, &enrichment);

    // v4 FAST ranking columns — always written so every doc has values.
    let (quality_ppm, downloads, popularity_pct_ppm) =
        ranking_signals_from_facets(record.facets.as_ref());
    document.add_u64(fields.quality_ppm, quality_ppm);
    document.add_u64(fields.downloads, downloads);
    document.add_u64(fields.popularity_pct_ppm, popularity_pct_ppm);

    if let Some(facets) = &record.facets {
        if let Some(description) = &facets.description {
            document.add_text(fields.description, description.as_str());
        }
        if !keywords_text.is_empty() {
            document.add_text(fields.keywords, &keywords_text);
        }
        // v3 facet filter fields.
        for dependency in &facets.dependencies {
            document.add_text(fields.deps, dependency.as_str());
        }
        if let Some(license) = &facets.license {
            // Schema contract: license is stored as a single lowercase term.
            document.add_text(fields.license, license.as_str().to_ascii_lowercase());
        }
        if let Some(repository) = &facets.repo_slug {
            document.add_text(fields.repo, repository.as_str());
        }
    } else if !keywords_text.is_empty() {
        // No facets yet: still index name-derived extras so dash-split
        // keyword matching works before rich metadata lands.
        document.add_text(fields.keywords, &keywords_text);
    }

    document.add_text(fields.record, &json);
    Ok(document)
}

fn load_watermark(directory: &std::path::Path) -> SyncWatermark {
    let path = directory.join(WATERMARK_FILE);
    match std::fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice::<SyncWatermark>(&bytes) {
            Ok(watermark) => watermark,
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %error,
                    "corrupt tantivy watermark; resuming from zero"
                );
                SyncWatermark { position: 0 }
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => SyncWatermark { position: 0 },
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                error = %error,
                "failed to read tantivy watermark; resuming from zero"
            );
            SyncWatermark { position: 0 }
        }
    }
}

fn persist_watermark(directory: &std::path::Path, watermark: SyncWatermark) {
    let path = directory.join(WATERMARK_FILE);
    let Ok(bytes) = serde_json::to_vec(&watermark) else {
        return;
    };
    let temporary = path.with_extension("json.tmp");
    if let Err(error) = std::fs::write(&temporary, &bytes) {
        tracing::warn!(
            path = %temporary.display(),
            error = %error,
            "failed to write tantivy watermark"
        );
        return;
    }
    if let Err(error) = std::fs::rename(&temporary, &path) {
        tracing::warn!(
            path = %path.display(),
            error = %error,
            "failed to persist tantivy watermark"
        );
    }
}

fn stored_text(document: &tantivy::TantivyDocument, field: Field) -> Result<String, SearchError> {
    match document.get_first(field) {
        Some(tantivy::schema::OwnedValue::Str(text)) => Ok(text.clone()),
        _ => Err(SearchError::TantivyInternal(
            tantivy::TantivyError::InternalError(
                "stored document is missing a schema-required text field".into(),
            ),
        )),
    }
}
