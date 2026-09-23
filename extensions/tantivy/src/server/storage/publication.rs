//! Content-addressed filesystem publication and projection reopen validation.
use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use backend_semantic::index_core::{EntityDocumentId, LexicalSegment, LexicalSegmentId, MAX_LEXICAL_ROWS};
use tantivy::{
    Index, IndexReader, doc,
    schema::{FieldType, IndexRecordOption, STORED, STRING, Schema, Type, Value},
};

use super::{
    codec,
    model::{StorePhase, TantivySegment, TantivySegmentStoreError},
    store::{backend_error, io_error},
};

const INDEX_DIR: &str = "tantivy";
const RECIPE_DIR: &str = "ntvx-v3";
const WRITER_MEMORY_BYTES: usize = 15_000_000;
const RECIPE: &[u8] = b"ntvx-segment-v3/sentinel-hex-string-stored-bytes-ordinal";
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

pub(crate) fn ensure_recipe_root(root: &Path) -> io::Result<()> {
    fs::create_dir_all(root.join(RECIPE_DIR))
}

pub(crate) fn sync_recipe_root(root: &Path) -> io::Result<()> {
    sync_directory(&root.join(RECIPE_DIR))
}

pub(crate) fn segment_path(root: &Path, id: LexicalSegmentId) -> PathBuf {
    root.join(RECIPE_DIR).join(hex_id(id))
}

#[allow(
    clippy::needless_continue,
    reason = "collision retry must continue with a fresh cross-process name"
)]
pub(crate) fn new_temp_dir(
    root: &Path,
    id: LexicalSegmentId,
) -> Result<PathBuf, TantivySegmentStoreError> {
    for _ in 0..32 {
        let number = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = root.join(RECIPE_DIR).join(format!(
            ".{}.tmp-{}-{}",
            hex_id(id),
            std::process::id(),
            number
        ));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => return Err(io_error(StorePhase::CreateTemporary, &path, source)),
        }
    }
    Err(TantivySegmentStoreError::TemporaryNameExhausted)
}

pub(crate) fn quarantine_path(root: &Path, id: LexicalSegmentId) -> PathBuf {
    let number = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    root.join(RECIPE_DIR).join(format!(
        ".{}.corrupt-{}-{}",
        hex_id(id),
        std::process::id(),
        number
    ))
}

#[allow(
    clippy::ignored_unit_patterns,
    reason = "recipe writes are ordered through one bounded file handle"
)]
pub(crate) fn build_projection(
    path: &Path,
    segment: LexicalSegment<'_>,
) -> Result<(), TantivySegmentStoreError> {
    let rows = codec::stored_rows(segment);
    for row in &rows {
        codec::validate_term_len(row.term.len())?;
    }
    codec::write_sidecar(path, segment.id, &rows)?;
    let recipe_path = path.join("recipe");
    let mut recipe_file = fs::File::create(&recipe_path)
        .map_err(|source| io_error(StorePhase::WriteRecipe, &recipe_path, source))?;
    io::Write::write_all(&mut recipe_file, RECIPE)
        .and_then(|_| io::Write::flush(&mut recipe_file))
        .and_then(|_| recipe_file.sync_all())
        .map_err(|source| io_error(StorePhase::WriteRecipe, &recipe_path, source))?;
    let mut schema_builder = Schema::builder();
    let body_field = schema_builder.add_text_field("body", STRING | STORED);
    let document_field = schema_builder.add_bytes_field("document", STORED);
    let ordinal_field = schema_builder.add_u64_field("ordinal", STORED);
    let index_path = path.join(INDEX_DIR);
    fs::create_dir(&index_path)
        .map_err(|source| io_error(StorePhase::CreateIndex, &index_path, source))?;
    let index = Index::create_in_dir(&index_path, schema_builder.build())
        .map_err(|source| backend_error(StorePhase::CreateIndex, &index_path, source))?;
    let mut writer = index
        .writer(WRITER_MEMORY_BYTES)
        .map_err(|source| backend_error(StorePhase::Writer, &index_path, source))?;
    for (ordinal, row) in rows.iter().enumerate() {
        let text = codec::encode_term(&row.term)?;
        let bytes: [u8; backend_semantic::index_core::ENTITY_DOCUMENT_ID_BYTES] = row.document.into();
        let ordinal =
            u64::try_from(ordinal).map_err(|_| TantivySegmentStoreError::CountOverflow)?;
        writer.add_document(doc!(body_field => text, document_field => bytes.to_vec(), ordinal_field => ordinal))
            .map_err(|source| backend_error(StorePhase::AddDocument, &index_path, source))?;
    }
    writer
        .commit()
        .map_err(|source| backend_error(StorePhase::Commit, &index_path, source))?;
    // Merge and garbage-collection threads keep rewriting segment files after
    // `commit` returns. Join them before the fsync barrier, or the directory
    // published below is still changing underneath its readers.
    writer
        .wait_merging_threads()
        .map_err(|source| backend_error(StorePhase::Commit, &index_path, source))?;
    Index::open_in_dir(&index_path)
        .map_err(|source| backend_error(StorePhase::Reopen, &index_path, source))?;
    sync_directory(&index_path)
        .map_err(|source| io_error(StorePhase::Publish, &index_path, source))?;
    sync_directory(path).map_err(|source| io_error(StorePhase::Publish, path, source))
}

#[allow(
    clippy::verbose_file_reads,
    reason = "recipe metadata bounds this validation read"
)]
pub(crate) fn open_segment(
    path: &Path,
    expected: LexicalSegmentId,
) -> Result<TantivySegment, TantivySegmentStoreError> {
    let rows = codec::read_sidecar(path, expected)?;
    let recipe = path.join("recipe");
    let metadata = fs::metadata(&recipe).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            TantivySegmentStoreError::Corrupt {
                path: recipe.clone(),
                detail: "projection recipe missing",
            }
        } else {
            io_error(StorePhase::ReadRecipe, &recipe, source)
        }
    })?;
    let recipe_max =
        u64::try_from(RECIPE.len()).map_err(|_| TantivySegmentStoreError::CountOverflow)?;
    if !metadata.is_file() || metadata.len() > recipe_max {
        return Err(TantivySegmentStoreError::Corrupt {
            path: recipe.clone(),
            detail: "projection recipe is not a bounded regular file",
        });
    }
    let mut observed_recipe = Vec::with_capacity(
        usize::try_from(metadata.len()).map_err(|_| TantivySegmentStoreError::CountOverflow)?,
    );
    fs::File::open(&recipe)
        .and_then(|mut file| file.read_to_end(&mut observed_recipe))
        .map_err(|source| io_error(StorePhase::ReadRecipe, &recipe, source))?;
    if observed_recipe != RECIPE {
        return Err(TantivySegmentStoreError::Corrupt {
            path: recipe,
            detail: "projection recipe mismatch",
        });
    }
    let index_path = path.join(INDEX_DIR);
    if !index_path.is_dir() {
        return Err(TantivySegmentStoreError::Corrupt {
            path: index_path,
            detail: "Tantivy projection directory missing",
        });
    }
    let index = Index::open_in_dir(&index_path)
        .map_err(|source| backend_error(StorePhase::Reopen, &index_path, source))?;
    let schema = index.schema();
    let body_field = schema
        .get_field("body")
        .map_err(|_| TantivySegmentStoreError::Corrupt {
            path: index_path.clone(),
            detail: "body field missing",
        })?;
    let document_field =
        schema
            .get_field("document")
            .map_err(|_| TantivySegmentStoreError::Corrupt {
                path: index_path.clone(),
                detail: "document field missing",
            })?;
    let ordinal_field =
        schema
            .get_field("ordinal")
            .map_err(|_| TantivySegmentStoreError::Corrupt {
                path: index_path.clone(),
                detail: "ordinal field missing",
            })?;
    let body_recipe_ok = matches!(schema.get_field_entry(body_field).field_type(), FieldType::Str(options) if options.get_indexing_options().is_some_and(|indexing| indexing.tokenizer() == "raw" && indexing.index_option() == IndexRecordOption::Basic && indexing.fieldnorms()));
    if schema.num_fields() != 3
        || !body_recipe_ok
        || !schema.get_field_entry(body_field).is_stored()
        || !schema
            .get_field_entry(document_field)
            .field_type()
            .is_bytes()
        || !schema.get_field_entry(document_field).is_stored()
        || schema
            .get_field_entry(ordinal_field)
            .field_type()
            .value_type()
            != Type::U64
        || !schema.get_field_entry(ordinal_field).is_stored()
    {
        return Err(TantivySegmentStoreError::Corrupt {
            path: index_path.clone(),
            detail: "schema options disagree with projection recipe",
        });
    }
    let reader = index
        .reader()
        .map_err(|source| backend_error(StorePhase::Reader, &index_path, source))?;
    validate_documents(
        &reader,
        &index_path,
        body_field,
        document_field,
        ordinal_field,
        &rows,
    )?;
    Ok(TantivySegment {
        id: expected,
        path: path.to_path_buf(),
        reader,
        body_field,
        ordinal_field,
        rows,
    })
}

fn validate_documents(
    reader: &IndexReader,
    index_path: &Path,
    body_field: tantivy::schema::Field,
    document_field: tantivy::schema::Field,
    ordinal_field: tantivy::schema::Field,
    rows: &[codec::StoredRow],
) -> Result<(), TantivySegmentStoreError> {
    let searcher = reader.searcher();
    let expected_count =
        u64::try_from(rows.len()).map_err(|_| TantivySegmentStoreError::CountOverflow)?;
    if searcher.num_docs() != expected_count {
        return Err(TantivySegmentStoreError::Corrupt {
            path: index_path.to_path_buf(),
            detail: "document count disagrees with canonical sidecar",
        });
    }
    let mut observed = [false; MAX_LEXICAL_ROWS];
    for (segment_ord, segment_reader) in searcher.segment_readers().iter().enumerate() {
        for doc_id in 0..segment_reader.max_doc() {
            let segment_ord =
                u32::try_from(segment_ord).map_err(|_| TantivySegmentStoreError::CountOverflow)?;
            let document: tantivy::TantivyDocument = searcher
                .doc(tantivy::DocAddress::new(segment_ord, doc_id))
                .map_err(|source| backend_error(StorePhase::ReadDocument, index_path, source))?;
            let ordinal = document
                .get_first(ordinal_field)
                .and_then(|value| value.as_u64())
                .and_then(|value| usize::try_from(value).ok())
                .ok_or(TantivySegmentStoreError::Corrupt {
                    path: index_path.to_path_buf(),
                    detail: "stored ordinal missing",
                })?;
            let expected = rows.get(ordinal).ok_or(TantivySegmentStoreError::Corrupt {
                path: index_path.to_path_buf(),
                detail: "stored ordinal out of range",
            })?;
            let seen = observed
                .get_mut(ordinal)
                .ok_or(TantivySegmentStoreError::Corrupt {
                    path: index_path.to_path_buf(),
                    detail: "stored ordinal out of range",
                })?;
            if *seen {
                return Err(TantivySegmentStoreError::Corrupt {
                    path: index_path.to_path_buf(),
                    detail: "stored ordinal duplicated",
                });
            }
            *seen = true;
            let term = document
                .get_first(body_field)
                .and_then(|value| value.as_str())
                .ok_or(TantivySegmentStoreError::Corrupt {
                    path: index_path.to_path_buf(),
                    detail: "stored term missing",
                })?;
            let bytes = document
                .get_first(document_field)
                .and_then(|value| value.as_bytes())
                .ok_or(TantivySegmentStoreError::Corrupt {
                    path: index_path.to_path_buf(),
                    detail: "stored document missing",
                })?;
            let entity = EntityDocumentId::try_from(bytes).map_err(|_| {
                TantivySegmentStoreError::Corrupt {
                    path: index_path.to_path_buf(),
                    detail: "stored document malformed",
                }
            })?;
            if term.as_bytes() != codec::encode_term(&expected.term)?.as_bytes()
                || entity != expected.document
            {
                return Err(TantivySegmentStoreError::Corrupt {
                    path: index_path.to_path_buf(),
                    detail: "stored projection facts disagree with sidecar",
                });
            }
        }
    }
    if observed.iter().take(rows.len()).any(|seen| !seen) {
        return Err(TantivySegmentStoreError::Corrupt {
            path: index_path.to_path_buf(),
            detail: "stored projection facts disagree with sidecar",
        });
    }
    Ok(())
}

pub(crate) fn sync_directory(path: &Path) -> io::Result<()> {
    backend_platform::durability::open_directory(path)?.sync_all()
}

#[allow(
    clippy::format_collect,
    reason = "the fixed identity width makes this allocation bounded and explicit"
)]
fn hex_id(id: LexicalSegmentId) -> String {
    id.as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
