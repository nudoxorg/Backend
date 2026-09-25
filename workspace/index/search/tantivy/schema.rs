//! Schema v4 field handles and the FAST ranking columns.

use tantivy::{Index, schema::Field};

use crate::error::SearchError;

use super::FastRankingSignals;

pub(super) const SCHEMA_VERSION: u32 = 4;
pub(super) const WATERMARK_FILE: &str = "sync_watermark.json";
pub(super) const SCHEMA_VERSION_FILE: &str = "schema_version";
pub(super) const WRITER_HEAP_BYTES: usize = 50 << 20;
pub(super) const SYNC_BATCH: u64 = 1024;

/// All query boost values in one place so tuning is one-line.
pub(super) struct PackageQueryBoosts;

impl PackageQueryBoosts {
    pub(super) const EXACT: f32 = 4.0;
    pub(super) const NAME_TOKENS: f32 = 2.0;
    pub(super) const NAME_NS: f32 = 1.5;
    pub(super) const DESCRIPTION: f32 = 1.0;
    pub(super) const KEYWORDS: f32 = 1.2;
    pub(super) const FUZZY: f32 = 0.3;
    pub(super) const EXPANDED: f32 = 0.5;
}

/// Schema field handles — resolved once at open so writer and reader always
/// agree.
#[derive(Clone, Copy)]
pub(super) struct Fields {
    pub(super) package_id: Field,
    pub(super) name_exact: Field,
    pub(super) name_tokens: Field,
    pub(super) name_ns: Field,
    pub(super) description: Field,
    pub(super) keywords: Field,
    pub(super) ecosystem: Field,
    pub(super) record: Field,
    pub(super) deps: Field,
    pub(super) license: Field,
    pub(super) repo: Field,
    pub(super) quality_ppm: Field,
    pub(super) downloads: Field,
    pub(super) popularity_pct_ppm: Field,
}

/// Schema v4: v3 text/facet filters plus FAST ranking columns.
pub(super) fn schema() -> tantivy::schema::Schema {
    use tantivy::schema::{
        FAST, IndexRecordOption, STORED, STRING, TEXT, TextFieldIndexing, TextOptions,
    };

    let mut builder = tantivy::schema::Schema::builder();
    builder.add_text_field("package_id", STRING | STORED);
    builder.add_text_field("name_exact", STRING);

    let identifier_options = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(crate::runtime::text::tokenizer::IDENT_TOKENIZER)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );
    builder.add_text_field("name_tokens", identifier_options.clone());
    builder.add_text_field("name_ns", identifier_options);
    builder.add_text_field("description", TEXT);
    builder.add_text_field("keywords", TEXT);
    builder.add_text_field("ecosystem", STRING | STORED);
    builder.add_text_field("record", STORED);
    builder.add_text_field("deps", STRING);
    builder.add_text_field("license", STRING);
    builder.add_text_field("repo", STRING);
    builder.add_u64_field("quality_ppm", FAST);
    builder.add_u64_field("downloads", FAST);
    builder.add_u64_field("popularity_pct_ppm", FAST);
    builder.build()
}

pub(super) fn resolve_fields(index: &Index) -> Result<Fields, SearchError> {
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

/// Always returns concrete values (0 when facets are absent).
pub(super) fn ranking_signals_from_facets(
    facets: Option<&crate::metadata::SearchFacets>,
) -> FastRankingSignals {
    facets.map_or(
        FastRankingSignals {
            quality_ppm: 0,
            downloads: 0,
            popularity_pct_ppm: 0,
        },
        |facets| {
            let quality_ppm = u64::from(facets.quality_ppm);
            let downloads = facets.downloads.unwrap_or(0);
            // popularity_pct is 0..=10_000. Scale by 100 → 0..=1_000_000 so it
            // shares a domain with quality_ppm.
            let popularity_pct_ppm = facets
                .popularity_pct
                .map_or(0, |value| u64::from(value.min(10_000)) * 100);
            FastRankingSignals {
                quality_ppm,
                downloads,
                popularity_pct_ppm,
            }
        },
    )
}
