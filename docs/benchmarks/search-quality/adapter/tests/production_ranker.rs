use backend_local_service::search_benchmark;
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn test_root(name: &str) -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    std::env::temp_dir().join(format!(
        "search-quality-{name}-{}-{timestamp}",
        std::process::id()
    ))
}

fn document(document_id: &str, name: &str, aliases: Value) -> Value {
    json!({
        "data_class": "adversarial-fixture",
        "document_id": document_id,
        "ecosystem": "cargo",
        "name": name,
        "version": "1.0.0",
        "coordinate": format!("fixture:cargo/{name}@1.0.0"),
        "source_kind": "registry-fixture",
        "source_snapshot_id": "workspace-fixture:search-benchmark-test",
        "archive_url": null,
        "archive_digest": null,
        "fields": {
            "aliases": aliases,
            "keywords": {"status": "unknown", "values": [], "source_snapshot_id": null},
            "description": {"status": "unknown", "values": [], "source_snapshot_id": null},
            "readme": {"status": "unknown", "values": [], "source_snapshot_id": null},
            "dependencies": {"status": "unknown", "values": [], "source_snapshot_id": null},
            "advisories": {"status": "unknown", "values": [], "source_snapshot_id": null},
            "yanked": {"status": "unknown", "values": [], "source_snapshot_id": null}
        }
    })
}

fn alias_query() -> Value {
    json!({
        "op": "search",
        "query_id": "alias.bench-alias",
        "query": "bench-alias",
        "category": "alias",
        "corpus": "adversarial",
        "scope": {"ecosystems": ["cargo"]},
        "exclude_yanked": false,
        "limit": 10
    })
}

fn forge_query(name: &str) -> Value {
    json!({
        "op": "search",
        "query_id": "exact.forge.fixture",
        "query": name,
        "category": "forge_only",
        "corpus": "primary",
        "scope": {"ecosystems": ["forge"]},
        "exclude_yanked": false,
        "limit": 10
    })
}

fn document_ids(response: &Value) -> Vec<String> {
    response
        .get("document_ids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

#[test]
fn production_ranker_returns_the_canonical_id_and_reports_index_bytes() {
    let root = test_root("canonical-id");
    let corpus_path = root.join("input.jsonl");
    let index_path = root.join("index");
    fs::create_dir_all(&root).expect("create test directory");
    let target = document(
        "fixture:cargo/bench-target@1.0.0",
        "bench-target",
        json!({"status": "known", "values": ["bench-alias"], "source_snapshot_id": "workspace-fixture:search-benchmark-test"}),
    );
    let distractor = document(
        "fixture:cargo/other-package@1.0.0",
        "other-package",
        json!({"status": "absent", "values": [], "source_snapshot_id": "workspace-fixture:search-benchmark-test"}),
    );
    let corpus = [target, distractor]
        .iter()
        .map(|row| serde_json::to_string(row))
        .collect::<Result<Vec<_>, _>>()
        .expect("encode fixture corpus")
        .join("\n")
        + "\n";
    fs::write(&corpus_path, corpus).expect("write fixture corpus");

    let report = search_benchmark::build(&corpus_path, &index_path)
        .expect("build actual production search index");
    assert_eq!(report["indexed_documents"], 2);
    assert!(
        report["index_bytes"]
            .as_u64()
            .is_some_and(|bytes| bytes > 0)
    );
    let index = search_benchmark::open(&index_path).expect("open production search index");
    let response = index
        .search_request(&alias_query())
        .expect("search through production ranker");
    assert_eq!(
        document_ids(&response),
        ["fixture:cargo/bench-target@1.0.0"]
    );
    drop(index);
    fs::remove_dir_all(root).expect("remove test directory");
}

#[test]
fn production_ranker_applies_alias_changes_through_incremental_sync() {
    let root = test_root("incremental-update");
    let corpus_path = root.join("input.jsonl");
    let index_path = root.join("index");
    fs::create_dir_all(&root).expect("create test directory");
    let target_id = "fixture:cargo/bench-target@1.0.0";
    let target = document(
        target_id,
        "bench-target",
        json!({"status": "known", "values": ["bench-alias"], "source_snapshot_id": "workspace-fixture:search-benchmark-test"}),
    );
    fs::write(
        &corpus_path,
        serde_json::to_string(&target).expect("encode target") + "\n",
    )
    .expect("write fixture corpus");
    search_benchmark::build(&corpus_path, &index_path)
        .expect("build actual production search index");
    let mut index = search_benchmark::open(&index_path).expect("open production search index");
    assert_eq!(
        document_ids(
            &index
                .search_request(&alias_query())
                .expect("search before update")
        ),
        [target_id]
    );

    let update = json!({
        "op": "update",
        "updates": [{
            "document_id": target_id,
            "fields": {
                "aliases": {
                    "field": "aliases",
                    "status": "absent",
                    "values": [],
                    "source_snapshot_id": "workspace-fixture:search-benchmark-test"
                }
            }
        }]
    });
    assert_eq!(
        index
            .update_request(&update)
            .expect("apply one-field update through production sync")["updated"],
        1
    );
    assert!(
        document_ids(
            &index
                .search_request(&alias_query())
                .expect("search after update")
        )
        .is_empty()
    );
    assert!(
        index
            .tantivy_projection_bytes()
            .expect("measure Tantivy managed files")
            > 0
    );
    drop(index);
    fs::remove_dir_all(root).expect("remove test directory");
}

#[test]
fn reopened_ranker_replays_yanked_overlay_and_update_cursor() {
    let root = test_root("reopen-yanked-overlay");
    let corpus_path = root.join("input.jsonl");
    let index_path = root.join("index");
    fs::create_dir_all(&root).expect("create test directory");
    let target_id = "fixture:cargo/bench-target@1.0.0";
    let target = document(
        target_id,
        "bench-target",
        json!({"status": "known", "values": ["bench-alias"], "source_snapshot_id": "workspace-fixture:search-benchmark-test"}),
    );
    fs::write(
        &corpus_path,
        serde_json::to_string(&target).expect("encode target") + "\n",
    )
    .expect("write fixture corpus");
    search_benchmark::build(&corpus_path, &index_path)
        .expect("build actual production search index");

    let mut index = search_benchmark::open(&index_path).expect("open production search index");
    let mut query = alias_query();
    query["exclude_yanked"] = json!(true);
    assert_eq!(
        document_ids(&index.search_request(&query).expect("search before yank")),
        [target_id]
    );
    let set_yanked = json!({
        "op": "update",
        "updates": [{
            "document_id": target_id,
            "fields": {"yanked": {"status": "known", "values": [true]}}
        }]
    });
    index
        .update_request(&set_yanked)
        .expect("persist yanked update");
    assert!(document_ids(&index.search_request(&query).expect("filter yanked update")).is_empty());
    drop(index);

    let mut reopened = search_benchmark::open(&index_path).expect("reopen production search index");
    assert!(document_ids(
        &reopened
            .search_request(&query)
            .expect("filter replayed yanked update")
    )
    .is_empty());
    let clear_yanked = json!({
        "op": "update",
        "updates": [{
            "document_id": target_id,
            "fields": {"yanked": {"status": "known", "values": [false]}}
        }]
    });
    reopened
        .update_request(&clear_yanked)
        .expect("continue with the recovered update cursor");
    assert_eq!(
        document_ids(
            &reopened
                .search_request(&query)
                .expect("search after unyank")
        ),
        [target_id]
    );
    drop(reopened);
    fs::remove_dir_all(root).expect("remove test directory");
}

#[test]
fn production_ranker_indexes_pinned_forge_identity_without_invented_metadata() {
    let root = test_root("forge-pin");
    let corpus_path = root.join("input.jsonl");
    let index_path = root.join("index");
    fs::create_dir_all(&root).expect("create test directory");
    let pinned_forge: Value = include_str!("../../primary-corpus.jsonl")
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .find(|row: &Value| row.get("source_kind").and_then(Value::as_str) == Some("forge"))
        .expect("frozen primary corpus contains a source-only forge pin");
    let target_id = pinned_forge["document_id"]
        .as_str()
        .expect("pinned forge row has a canonical benchmark id");
    let name = pinned_forge["name"]
        .as_str()
        .expect("pinned forge row has a package name");
    fs::write(
        &corpus_path,
        serde_json::to_string(&pinned_forge).expect("encode forge pin") + "\n",
    )
    .expect("write forge pin corpus");

    let report = search_benchmark::build(&corpus_path, &index_path)
        .expect("build actual forge-capable production index");
    assert_eq!(report["indexed_documents"], 1);
    assert_eq!(
        report["skipped_documents"].as_array().map(Vec::len),
        Some(0)
    );
    let index = search_benchmark::open(&index_path).expect("open production search index");
    assert_eq!(
        document_ids(
            &index
                .search_request(&forge_query(name))
                .expect("search pinned forge package")
        ),
        [target_id]
    );
    drop(index);
    fs::remove_dir_all(root).expect("remove test directory");
}
