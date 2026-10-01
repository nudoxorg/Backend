include!(concat!(env!("OUT_DIR"), "/upstream_search_index.rs"));

use rich_crate::Origin;
use semver::Version;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::Instant;
use upstream_search_index::{CrateSearchIndex, Indexer};

#[derive(Clone)]
struct Document {
    id: String,
    name: String,
    version: String,
    description: String,
    keywords: Vec<String>,
}

fn jsonl(path: &Path) -> Result<Vec<Value>, String> {
    let file = fs::File::open(path).map_err(|error| format!("open {}: {error}", path.display()))?;
    BufReader::new(file)
        .lines()
        .enumerate()
        .filter_map(|(line, row)| match row {
            Ok(row) if row.trim().is_empty() => None,
            Ok(row) => Some(
                serde_json::from_str(&row)
                    .map_err(|error| format!("{}:{}: {error}", path.display(), line + 1)),
            ),
            Err(error) => Some(Err(format!("{}:{}: {error}", path.display(), line + 1))),
        })
        .collect()
}

fn facet_string(row: &Value, field: &str) -> String {
    row.get("fields")
        .and_then(|fields| fields.get(field))
        .and_then(|facet| facet.get("values"))
        .and_then(Value::as_array)
        .and_then(|values| values.first())
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn facet_strings(row: &Value, field: &str) -> Vec<String> {
    row.get("fields")
        .and_then(|fields| fields.get(field))
        .and_then(|facet| facet.get("values"))
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn facet_status(row: &Value, field: &str) -> String {
    row.get("field_status")
        .and_then(|status| status.get(field))
        .and_then(Value::as_str)
        .or_else(|| {
            row.get("fields")
                .and_then(|fields| fields.get(field))
                .and_then(|facet| facet.get("status"))
                .and_then(Value::as_str)
        })
        .unwrap_or("unknown")
        .to_owned()
}

fn parse_document(row: &Value) -> Result<Document, String> {
    let id = row
        .get("document_id")
        .and_then(Value::as_str)
        .ok_or("document_id missing")?
        .to_owned();
    let name = row
        .get("crate_name")
        .or_else(|| row.get("name"))
        .and_then(Value::as_str)
        .ok_or_else(|| format!("crate name missing: {id}"))?
        .to_owned();
    let version = row
        .get("version")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| id.rsplit_once('@').map(|(_, version)| version.to_owned()))
        .ok_or_else(|| format!("version missing: {id}"))?;
    Ok(Document {
        id,
        name,
        version,
        description: row
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| facet_string(row, "description")),
        keywords: row
            .get("keywords")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_else(|| facet_strings(row, "keywords")),
    })
}

fn directory_bytes(path: &Path) -> Result<u64, String> {
    let mut total = 0u64;
    for entry in fs::read_dir(path).map_err(|error| format!("read {}: {error}", path.display()))? {
        let entry = entry.map_err(|error| error.to_string())?;
        let metadata = entry.metadata().map_err(|error| error.to_string())?;
        if metadata.is_dir() {
            total += directory_bytes(&entry.path())?;
        } else {
            total += metadata.len();
        }
    }
    Ok(total)
}

fn write_atomic_json(path: &Path, value: &Value) -> Result<(), String> {
    let parent = path.parent().ok_or("output path has no parent directory")?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    let mut file = fs::File::create(&temporary)
        .map_err(|error| format!("create {}: {error}", temporary.display()))?;
    use std::io::Write as _;
    file.write_all(&bytes)
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("persist {}: {error}", temporary.display()))?;
    fs::rename(&temporary, path).map_err(|error| format!("replace {}: {error}", path.display()))?;
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("sync {}: {error}", parent.display()))
}

fn main() -> Result<(), String> {
    let mut args = env::args().skip(1);
    if args.next().as_deref() == Some("search-once") {
        return search_once(args);
    }
    let mut args = env::args().skip(1);
    let documents_path = PathBuf::from(
        args.next()
            .ok_or("usage: harness DOCUMENTS_JSONL QUERIES_JSONL DATA_DIR OUTPUT_JSON")?,
    );
    let queries_path = PathBuf::from(args.next().ok_or("queries path missing")?);
    let data_dir = PathBuf::from(args.next().ok_or("data directory missing")?);
    let output_path = PathBuf::from(args.next().ok_or("output path missing")?);
    let search_limit = args
        .next()
        .as_deref()
        .unwrap_or("150")
        .parse::<usize>()
        .map_err(|error| format!("invalid search limit: {error}"))?;
    let sample_count = args
        .next()
        .as_deref()
        .unwrap_or("1001")
        .parse::<usize>()
        .map_err(|error| format!("invalid sample count: {error}"))?;
    let resume = args.next().as_deref() == Some("resume");
    fs::create_dir_all(&data_dir).map_err(|error| error.to_string())?;
    let synonyms_path = data_dir.join("tag-synonyms.csv");
    if !synonyms_path.exists() {
        fs::write(&synonyms_path, b"").map_err(|error| error.to_string())?;
    }

    let input_rows = jsonl(&documents_path)?;
    let mut documents = input_rows
        .iter()
        .map(parse_document)
        .collect::<Result<Vec<_>, _>>()?;
    if documents.is_empty() {
        return Err("common-field Cargo corpus is empty".to_owned());
    }
    let input_count = documents.len();
    let mut grouped: BTreeMap<String, Vec<Document>> = BTreeMap::new();
    for document in documents.drain(..) {
        grouped
            .entry(document.name.to_ascii_lowercase())
            .or_default()
            .push(document);
    }
    let mut selected = Vec::new();
    for rows in grouped.values_mut() {
        rows.sort_by(|left, right| {
            Version::parse(&left.version)
                .expect("frozen Cargo version")
                .cmp(&Version::parse(&right.version).expect("frozen Cargo version"))
        });
        selected.push(rows.last().expect("group is nonempty").clone());
    }
    selected.sort_by(|left, right| left.name.cmp(&right.name));

    let index_path = data_dir.join("tantivy18");
    let index_build_started = Instant::now();
    if index_path.exists() {
        fs::remove_dir_all(&index_path)
            .map_err(|error| format!("reset {}: {error}", index_path.display()))?;
    }
    let mut index = CrateSearchIndex::new(&data_dir)
        .map_err(|error| format!("open upstream index: {error}"))?;
    let mut writer =
        Indexer::new(index).map_err(|error| format!("create upstream indexer: {error}"))?;
    for document in &selected {
        let keywords = document
            .keywords
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        writer
            .add(
                &Origin::from_crates_io_name(&document.name),
                &document.name,
                &document.version,
                &document.description,
                &keywords,
                None,
                0,
                1.0,
            )
            .map_err(|error| format!("index {}: {error}", document.id))?;
    }
    writer
        .commit()
        .map_err(|error| format!("commit upstream index: {error}"))?;
    index = writer
        .bye()
        .map_err(|error| format!("close upstream index writer: {error}"))?;
    let index_build_elapsed_ns = index_build_started.elapsed().as_nanos() as u64;
    let index_bytes = directory_bytes(&data_dir.join("tantivy18"))?;

    let query_rows = jsonl(&queries_path)?;
    let queries = query_rows
        .iter()
        .filter(|row| row.get("lib_rs_common").and_then(Value::as_bool) == Some(true))
        .collect::<Vec<_>>();
    let ids_by_coordinate = selected
        .iter()
        .map(|document| {
            (
                format!(
                    "{}@{}",
                    document.name.to_ascii_lowercase(),
                    document.version
                ),
                document.id.clone(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let ids_path = data_dir.join("ids-by-coordinate.json");
    fs::write(
        &ids_path,
        serde_json::to_vec(&ids_by_coordinate).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("write {}: {error}", ids_path.display()))?;
    let existing_results = if resume && output_path.is_file() {
        let bytes = fs::read(&output_path)
            .map_err(|error| format!("read previous query checkpoints: {error}"))?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("decode previous query checkpoints: {error}"))?;
        value
            .get("results")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|row| Some((row.get("query_id")?.as_str()?.to_owned(), row.clone())))
            .collect::<BTreeMap<_, _>>()
    } else {
        BTreeMap::new()
    };
    let field_counts = ["crate_name", "keywords", "description"]
        .into_iter()
        .map(|field| {
            let known = input_rows
                .iter()
                .filter(|row| {
                    if field == "crate_name" {
                        true
                    } else {
                        facet_status(row, field) == "known"
                    }
                })
                .count();
            (
                field.to_owned(),
                json!({"known": known, "unknown": input_count - known, "total": input_count}),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let indexed_document_ids = selected
        .iter()
        .map(|document| document.id.clone())
        .collect::<Vec<_>>();
    let mut results = Vec::new();
    for query in queries {
        let query_id = query
            .get("query_id")
            .and_then(Value::as_str)
            .ok_or("query_id missing")?;
        let text = query
            .get("query")
            .and_then(Value::as_str)
            .ok_or("query missing")?;
        if let Some(previous) = existing_results.get(query_id) {
            let complete = previous.get("query").and_then(Value::as_str) == Some(text)
                && previous
                    .get("warm_latency_ns")
                    .and_then(Value::as_array)
                    .is_some_and(|samples| samples.len() == sample_count)
                && previous.get("ranked").and_then(Value::as_array).is_some();
            if complete {
                results.push(previous.clone());
                continue;
            }
        }
        let first = index
            .search(text, search_limit, true)
            .map_err(|error| format!("search {query_id}: {error}"))?;
        let mut ranked = Vec::new();
        for hit in first.crates {
            let key = format!("{}@{}", hit.crate_name.to_ascii_lowercase(), hit.version);
            ranked.push(json!({"document_id": ids_by_coordinate.get(&key), "crate_name": hit.crate_name, "version": hit.version, "relevance_score": hit.relevance_score, "score": hit.score}));
        }
        let mut latency_samples = Vec::with_capacity(sample_count);
        let sample_start = Instant::now();
        for _ in 0..sample_count {
            let started = Instant::now();
            let _ = index
                .search(text, search_limit, true)
                .map_err(|error| format!("timed search {query_id}: {error}"))?;
            latency_samples.push(started.elapsed().as_nanos() as u64);
        }
        let elapsed = sample_start.elapsed().as_nanos() as u64;
        results.push(json!({"query_id": query_id, "query": text, "ranked": ranked, "warm_latency_ns": latency_samples, "throughput_queries_per_second": sample_count as f64 * 1_000_000_000.0 / elapsed as f64}));
        let checkpoint = json!({
            "upstream_revision": "4642a01664e14f4ae30a3804a55556b0770119d9",
            "ranker": "pinned upstream search_index::CrateSearchIndex.search(sort_by_query_relevance=true)",
            "search_limit": search_limit,
            "sample_count": sample_count,
            "input_documents": input_count,
            "indexed_documents": selected.len(),
            "indexed_document_ids": indexed_document_ids.clone(),
            "results": results,
        });
        write_atomic_json(&output_path, &checkpoint)?;
    }
    let persistent_tantivy_directory_bytes = directory_bytes(&data_dir.join("tantivy18"))?;
    let persistent_data_directory_bytes = directory_bytes(&data_dir)?;
    let result = json!({
        "upstream_revision": "4642a01664e14f4ae30a3804a55556b0770119d9",
        "upstream_search_index_sha256": "24705598c93b49933013125e69e7cfdb39e7b4f47d4cdaa68ae4c47367f6e301",
        "upstream_ranking_sha256": "45e2b9fd0de65db22e6c3067f57be6e1c788c482571124992a162bd732fafcda",
        "ranker": "pinned upstream CrateSearchIndex.search(sort_by_query_relevance=true); common-field mode",
        "input_documents": input_count,
        "indexed_documents": selected.len(),
        "version_selection": "highest SemVer version in this frozen common Cargo corpus per Origin::CratesIo(name); upstream Indexer replaces documents by origin",
        "field_coverage": field_counts,
        "indexed_document_ids": indexed_document_ids,
        "index_build_elapsed_ns": index_build_elapsed_ns,
        "synthetic_neutral_indexer_arguments": {"crate_score": 1.0, "monthly_downloads": 0, "monthly_downloads_status": "unknown; API requires u64 and this is ignored by query-relevance ordering", "readme": "omitted"},
        "index_bytes": index_bytes,
        "persistent_tantivy_directory_bytes": persistent_tantivy_directory_bytes,
        "persistent_data_directory_bytes": persistent_data_directory_bytes,
        "results": results
    });
    write_atomic_json(&output_path, &result)?;
    println!("{}", output_path.display());
    Ok(())
}

fn search_once(mut args: impl Iterator<Item = String>) -> Result<(), String> {
    let data_dir = PathBuf::from(
        args.next()
            .ok_or("usage: harness search-once DATA_DIR QUERY LIMIT")?,
    );
    let query = args.next().ok_or("search-once query missing")?;
    let limit = args
        .next()
        .ok_or("search-once limit missing")?
        .parse::<usize>()
        .map_err(|error| format!("invalid search-once limit: {error}"))?;
    let index = CrateSearchIndex::new(&data_dir)
        .map_err(|error| format!("open upstream index: {error}"))?;
    let id_bytes = fs::read(data_dir.join("ids-by-coordinate.json"))
        .map_err(|error| format!("read ID map: {error}"))?;
    let ids_by_coordinate: BTreeMap<String, String> =
        serde_json::from_slice(&id_bytes).map_err(|error| format!("decode ID map: {error}"))?;
    let results = index
        .search(&query, limit, true)
        .map_err(|error| format!("search upstream index: {error}"))?;
    let ranked_document_ids = results
        .crates
        .into_iter()
        .filter_map(|hit| {
            ids_by_coordinate
                .get(&format!(
                    "{}@{}",
                    hit.crate_name.to_ascii_lowercase(),
                    hit.version
                ))
                .cloned()
        })
        .collect::<Vec<_>>();
    println!(
        "{}",
        serde_json::to_string(&json!({"ranked_document_ids": ranked_document_ids}))
            .map_err(|error| error.to_string())?
    );
    Ok(())
}
