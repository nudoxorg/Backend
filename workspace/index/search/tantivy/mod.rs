//! Replica-local tantivy index over the searchable package projection.
//!
//! Schema v4 (`SCHEMA_VERSION = 4`):
//! - `package_id` STRING|STORED — upsert/delete key
//! - `name_exact` STRING — search_surface, canonical, and original, lowercased
//! - `name_tokens` / `name_ns` TEXT(ident)
//! - `description`, `keywords`, `ecosystem`, `record`
//! - `deps`, `license`, `repo`
//! - `quality_ppm`, `downloads`, `popularity_pct_ppm` FAST u64
//!
//! A query is a [`plan::PackageQueryPlan`]. [`compile`] lowers that plan into
//! the boolean tree. FAST columns are for collectors and the round-trip
//! check; live ranking still hydrates the stored `record`.

mod compile;
mod document;
mod plan;
mod schema;

use heart::PackageId;
use tantivy::{Index, IndexReader};

use crate::{GlobalPackage, error::SearchError, runtime::text::tokenizer};

use schema::SCHEMA_VERSION;

/// The last catalog outbox position a replica has folded into its local index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SyncWatermark {
    pub position: i64,
}

/// Ranking signals stored as FAST u64 columns (schema v4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FastRankingSignals {
    /// Quality in parts-per-million (`0..=1_000_000`); `0` if facets unknown.
    pub quality_ppm: u64,
    /// Monthly downloads; `0` when facets omit downloads.
    pub downloads: u64,
    /// Ecosystem popularity percentile as ppm (`0..=1_000_000`); `0` if unset.
    pub popularity_pct_ppm: u64,
}

/// A replica-local tantivy index over the searchable package projection.
pub struct PackageIndex {
    index: Index,
    reader: IndexReader,
    fields: schema::Fields,
    watermark: SyncWatermark,
    dir: std::path::PathBuf,
}

impl PackageIndex {
    /// Open (or create) the replica-local index at `path`.
    ///
    /// On schema version mismatch the directory is wiped and the watermark
    /// resets to zero so a full resync rebuilds under the current schema.
    pub fn open(path: &std::path::Path) -> Result<Self, SearchError> {
        std::fs::create_dir_all(path)
            .map_err(|error| SearchError::Tantivy(tantivy::TantivyError::from(error)))?;
        maybe_wipe_for_schema_change(path);

        let directory = tantivy::directory::MmapDirectory::open(path)
            .map_err(|error| SearchError::Tantivy(error.into()))?;
        let index =
            Index::open_or_create(directory, schema::schema()).map_err(SearchError::Tantivy)?;
        tokenizer::register(&index);

        let reader = index.reader().map_err(SearchError::Tantivy)?;
        let fields = schema::resolve_fields(&index)?;
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

    /// Schema v4.
    pub fn schema() -> tantivy::schema::Schema {
        schema::schema()
    }

    /// Fold a batch of package records into the local index at `position`.
    pub fn absorb<'record>(
        &mut self,
        records: impl IntoIterator<Item = &'record GlobalPackage>,
        position: i64,
    ) -> Result<SyncWatermark, SearchError> {
        let mut writer = self
            .index
            .writer(schema::WRITER_HEAP_BYTES)
            .map_err(SearchError::Tantivy)?;
        let mut folded = 0usize;
        for record in records {
            let package_token = record.id.to_string();
            writer.delete_term(tantivy::Term::from_field_text(
                self.fields.package_id,
                &package_token,
            ));
            writer
                .add_document(document::build_document(&self.fields, record)?)
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

    /// Poll the catalog outbox (Text sink) from the current watermark.
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
                schema::SYNC_BATCH as usize,
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

    /// Raw-text query. Parses into a plan, then executes it.
    pub fn query(&self, text: &str, limit: usize) -> Result<Vec<(PackageId, f32)>, SearchError> {
        let structured = super::structured::StructuredQuery::parse(text, None);
        Ok(self
            .query_structured(&structured, limit)?
            .into_iter()
            .map(|(package, score)| (package.id, score))
            .collect())
    }

    /// Execute a structured query. The plan is pure; compilation is the only
    /// step that knows tantivy fields.
    pub fn query_structured(
        &self,
        structured: &super::structured::StructuredQuery,
        limit: usize,
    ) -> Result<Vec<(GlobalPackage, f32)>, SearchError> {
        let planned = plan::plan(structured);
        compile::search(&self.reader, &self.fields, &planned, limit)
    }

    /// Hydrate matched package ids from the stored projection.
    pub async fn hydrate(
        &self,
        identifiers: &[PackageId],
    ) -> Result<Vec<GlobalPackage>, SearchError> {
        compile::hydrate(&self.reader, &self.fields, identifiers)
    }

    /// Read FAST ranking signals for a package. `None` when it is absent.
    pub fn fast_ranking_signals(
        &self,
        identifier: PackageId,
    ) -> Result<Option<FastRankingSignals>, SearchError> {
        compile::fast_ranking_signals(&self.reader, &self.fields, identifier)
    }

    /// Snapshot of index health for ops dashboards.
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

    /// Remove a package's document by id and commit.
    pub fn remove(&mut self, package: PackageId) -> Result<(), SearchError> {
        let package_token = package.to_string();
        let mut writer: tantivy::IndexWriter<tantivy::TantivyDocument> = self
            .index
            .writer(schema::WRITER_HEAP_BYTES)
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

fn maybe_wipe_for_schema_change(path: &std::path::Path) {
    let marker_path = path.join(schema::SCHEMA_VERSION_FILE);
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
        let _ = std::fs::remove_file(path.join(schema::WATERMARK_FILE));
    }
    if std::fs::write(&marker_path, SCHEMA_VERSION.to_string()).is_err() {
        tracing::warn!(path = %marker_path.display(), "failed to write schema_version marker");
    }
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

fn load_watermark(directory: &std::path::Path) -> SyncWatermark {
    let path = directory.join(schema::WATERMARK_FILE);
    match std::fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice::<SyncWatermark>(&bytes) {
            Ok(watermark) => watermark,
            Err(error) => {
                tracing::warn!(path = %path.display(), error = %error, "corrupt tantivy watermark; resuming from zero");
                SyncWatermark { position: 0 }
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => SyncWatermark { position: 0 },
        Err(error) => {
            tracing::warn!(path = %path.display(), error = %error, "failed to read tantivy watermark; resuming from zero");
            SyncWatermark { position: 0 }
        }
    }
}

fn persist_watermark(directory: &std::path::Path, watermark: SyncWatermark) {
    let path = directory.join(schema::WATERMARK_FILE);
    let Ok(bytes) = serde_json::to_vec(&watermark) else {
        return;
    };
    let temporary = path.with_extension("json.tmp");
    if let Err(error) = std::fs::write(&temporary, &bytes) {
        tracing::warn!(path = %temporary.display(), error = %error, "failed to write tantivy watermark");
        return;
    }
    if let Err(error) = std::fs::rename(&temporary, &path) {
        tracing::warn!(path = %path.display(), error = %error, "failed to persist tantivy watermark");
    }
}
