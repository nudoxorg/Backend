//! Shard lifecycle: schema-validated open-or-create (09-vector §13.2,
//! §20.3; 09c §1.1 "beta format thrash" patch).
//!
//! Every shard directory carries a `schema.json` written by *us* (distinct
//! from Edge's own `edge_config.json`): format version, model identity,
//! dimensionality, distance, quant profile, recipe id. Opening validates it
//! against the caller's expectation and refuses with an actionable
//! [`StoreError::Corrupt`] on mismatch — never silently searches the wrong
//! geometry.
//!
//! Crash window: creation writes `schema.json` **last** (tmp + rename). A
//! crash between Edge shard creation and the schema write leaves a valid
//! Edge directory without our schema; recovery re-creates the payload
//! indexes (idempotent) and rewrites `schema.json`.

use std::fs;
use std::io::Write as _;
use std::path::Path;

use crate::vector::core::{
    EDGE_FORMAT_VERSION, EmbeddingModel, HnswParams, Metric, QuantProfile, RECIPE_ID, ShardSchema,
    StoreError,
};
use qdrant_edge::{
    CreateIndex, Distance, EdgeConfig, EdgeConfigBuilder, EdgeShard, EdgeVectorParamsBuilder,
    FieldIndexOperations, HnswIndexConfig, JsonPath, PayloadFieldSchema, PayloadSchemaType,
    QuantizationConfig, ScalarQuantizationConfig, ScalarType, UpdateOperation, WalOptions,
};

use super::{backend_error, corrupt_error};

/// Our schema sidecar (distinct from Edge's `edge_config.json`).
pub const SCHEMA_FILE: &str = "schema.json";

/// The single named dense vector of the local plane (09-vector §20.3).
pub const VECTOR_NAME: &str = "sym";

/// Keyword payload indexes created before any ingest (09-vector §13.2).
pub const PAYLOAD_INDEX_FIELDS: [&str; 3] = ["language", "package", "kind"];

/// Edge's default WAL segment capacity is 32 MiB — oversized for a desktop
/// per-project shard; 4 MiB keeps the write amplification and disk floor low.
const WAL_SEGMENT_CAPACITY: usize = 4 * 1024 * 1024;

/// Build the local-plane [`ShardSchema`] for a branded model.
///
/// This is the crate's single `ShardSchema` construction site (vector-core
/// integration point).
pub fn schema_for<M: EmbeddingModel>(quant_profile: QuantProfile) -> ShardSchema {
    ShardSchema {
        format_version: EDGE_FORMAT_VERSION,
        model_id: M::id(),
        dim: M::DIMENSIONS,
        distance: Metric::Cosine,
        quant_profile,
        recipe_id: RECIPE_ID.into(),
    }
}

/// Open the shard at `dir`, creating it with the frozen client config when
/// the directory is fresh.
///
/// - `schema.json` present → validate `format_version` / `model_id` / `dim`
///   against `expected` (mismatch → [`StoreError::Corrupt`] with a rebuild
///   instruction), then `EdgeShard::load`.
/// - Edge data present but no `schema.json` (crash between create and
///   schema write) → load, re-ensure payload indexes, rewrite the schema.
/// - Fresh directory → `EdgeShard::new` with named vector `"sym"`
///   (`expected.dim`, Cosine, `on_disk=true`), `on_disk_payload=true`,
///   quantization per `expected.quant_profile`, tuned WAL; then keyword
///   indexes; then `schema.json` atomically (tmp + rename), last.
pub fn open_or_create(dir: &Path, expected: &ShardSchema) -> Result<EdgeShard, StoreError> {
    fs::create_dir_all(dir).map_err(backend_error)?;

    let schema_path = dir.join(SCHEMA_FILE);
    if schema_path.exists() {
        let stored = read_schema(&schema_path)?;
        validate_schema(dir, &stored, expected)?;
        return EdgeShard::load(dir, None).map_err(backend_error);
    }

    if has_edge_data(dir) {
        // Torn creation: the Edge shard exists but our schema write never
        // landed. The shard was necessarily created by this same code with
        // `expected`'s config, so recover by finishing the interrupted steps.
        tracing::warn!(dir = %dir.display(), "shard missing schema.json; recovering from torn creation");
        let shard = EdgeShard::load(dir, None).map_err(backend_error)?;
        create_payload_indexes(&shard)?;
        write_schema_atomically(dir, expected)?;
        return Ok(shard);
    }

    let shard = EdgeShard::new(dir, edge_config(expected)).map_err(backend_error)?;
    create_payload_indexes(&shard)?;
    write_schema_atomically(dir, expected)?;
    Ok(shard)
}

/// The frozen client Edge config for a schema (09-vector §20.3 step 3).
fn edge_config(schema: &ShardSchema) -> EdgeConfig {
    let hnsw = HnswParams::default();
    let hnsw_config = HnswIndexConfig {
        m: hnsw.m,
        ef_construct: hnsw.ef_construct,
        ..HnswIndexConfig::default()
    };

    let mut vector = EdgeVectorParamsBuilder::new(schema.dim, Distance::Cosine).on_disk(true);
    if let Some(quantization) = quantization_config(&schema.quant_profile) {
        vector = vector.quantization_config(quantization);
    }

    EdgeConfigBuilder::new()
        .vector(VECTOR_NAME, vector.build())
        .on_disk_payload(true)
        .hnsw_config(hnsw_config)
        .wal_options(WalOptions {
            segment_capacity: WAL_SEGMENT_CAPACITY,
            ..WalOptions::default()
        })
        .build()
}

/// Map the vector-core quant profile onto Edge's quantization config.
pub(crate) fn quantization_config(profile: &QuantProfile) -> Option<QuantizationConfig> {
    match profile {
        QuantProfile::None => None,
        QuantProfile::ScalarInt8 {
            quantile,
            always_ram,
        } => Some(QuantizationConfig::from(ScalarQuantizationConfig {
            r#type: ScalarType::Int8,
            quantile: Some(*quantile),
            always_ram: Some(*always_ram),
        })),
    }
}

/// Create the keyword facet indexes. Idempotent; run before any ingest.
fn create_payload_indexes(shard: &EdgeShard) -> Result<(), StoreError> {
    for field in PAYLOAD_INDEX_FIELDS {
        let field_name: JsonPath = field
            .parse()
            .map_err(|()| backend_error(format!("invalid payload index key {field:?}")))?;
        shard
            .update(UpdateOperation::FieldIndexOperation(
                FieldIndexOperations::CreateIndex(CreateIndex {
                    field_name,
                    field_schema: Some(PayloadFieldSchema::FieldType(PayloadSchemaType::Keyword)),
                }),
            ))
            .map_err(backend_error)?;
    }
    Ok(())
}

fn read_schema(path: &Path) -> Result<ShardSchema, StoreError> {
    let bytes = fs::read(path).map_err(backend_error)?;
    serde_json::from_slice(&bytes).map_err(|err| {
		corrupt_error(format!(
			"unreadable {} at {}: {err}; delete the shard directory and re-index (or re-install the dep shard)",
			SCHEMA_FILE,
			path.display(),
		))
	})
}

fn validate_schema(
    dir: &Path,
    stored: &ShardSchema,
    expected: &ShardSchema,
) -> Result<(), StoreError> {
    if stored.format_version != expected.format_version {
        return Err(corrupt_error(format!(
            "shard at {} has edge format v{}, this build expects v{}; \
			 delete the directory and rebuild (project: re-embed; dep: re-install the baked shard)",
            dir.display(),
            stored.format_version,
            expected.format_version,
        )));
    }
    if stored.model_id != expected.model_id {
        return Err(corrupt_error(format!(
            "shard at {} was embedded with model {:?}, this build expects {:?}; \
			 vectors are incomparable across models — delete the directory and rebuild",
            dir.display(),
            stored.model_id,
            expected.model_id,
        )));
    }
    if stored.dim != expected.dim {
        return Err(corrupt_error(format!(
            "shard at {} stores {}-dimensional vectors, this build expects {}; \
			 delete the directory and rebuild",
            dir.display(),
            stored.dim,
            expected.dim,
        )));
    }
    Ok(())
}

/// Detect Edge state left by a creation that crashed before the schema write.
fn has_edge_data(dir: &Path) -> bool {
    dir.join("segments").is_dir()
        || dir.join("wal").is_dir()
        || dir.join("edge_config.json").is_file()
}

/// Write `schema.json` via tmp + fsync + rename so a crash never leaves a
/// torn schema next to a live shard.
fn write_schema_atomically(dir: &Path, schema: &ShardSchema) -> Result<(), StoreError> {
    let bytes = serde_json::to_vec_pretty(schema).map_err(backend_error)?;
    let tmp = dir.join(".schema.json.tmp");
    {
        let mut file = fs::File::create(&tmp).map_err(backend_error)?;
        file.write_all(&bytes).map_err(backend_error)?;
        file.sync_all().map_err(backend_error)?;
    }
    fs::rename(&tmp, dir.join(SCHEMA_FILE)).map_err(backend_error)?;
    Ok(())
}
