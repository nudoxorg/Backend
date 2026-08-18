//! Response-corpus dump for the MCP token proof.
//!
//! Renders every MCP tool response for a fixed set of realistic agent
//! workloads against the REAL corpus, and writes each one verbatim to
//! `$NUDOX_DUMP_DIR/<case>.md`. The dumps are then tokenized out-of-band with
//! a real BPE tokenizer (tiktoken), because `heart::cost::estimated_text_tokens`
//! counts an alphanumeric run as one token and therefore prices a 64-character
//! hex key at 1 token instead of ~50.
//!
//! This file is deliberately written against only the *stable* surface —
//! `NudoxTools::do_*` plus `mcp::render_markdown` — so the identical harness
//! runs against the pre-redesign baseline (a detached worktree at HEAD) and
//! against the redesigned tree, and the two dumps are directly comparable.
//!
//! # Running
//!
//! ```text
//! NUDOX_DUMP_DIR=/tmp/dump cargo test -p nudox-engine --test mcp_dump_responses \
//!     -- --ignored --nocapture
//! ```

use std::path::PathBuf;
use std::time::Duration;

use nudox_engine::mcp::key::PackageLineageDto;
use nudox_engine::mcp::tools::{
    FindUsagesArgs, GetSymbolsArgs, GraphQueryArgs, ListVersionsArgs, SearchSymbolsArgs,
    SymbolFormat,
};
use nudox_engine::mcp::{NudoxTools, SymbolKeyDto, render_markdown};
use nudox_engine::{
    Engine, EngineConfig, PackageHistorySpec, PackageLoadEvent, PackageVersionSpec,
    ProducerLanguage, SharedStr,
};
use tokio::runtime::Runtime;

/// Packages this harness loads. Kept small so the producer run stays bounded,
/// but real: `serde` is the canonical wide-API crate and `memchr` is the one
/// the corpus pins three generations of.
const PACKAGES: [(&str, &str); 2] = [("serde", "1.0.196"), ("memchr", "2.8.3")];

fn store_root(name: &str, version: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../result/{name}-{version}"))
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("/nonexistent"))
}

/// Copy a corpus package out of the nix store into a writable directory.
///
/// The corpus lives in `/nix/store`, which is read-only. Crates with a
/// `build.rs` — `serde` among them — need cargo to write a `Cargo.lock` beside
/// the manifest before their build scripts can run, and the producer refuses to
/// lower a crate whose build-script cfgs never reached the crate graph:
/// "the resulting table is not a smaller description of this crate, it is a
/// description of a different one." Accepting the missing cfgs instead would
/// measure a crate that does not exist, so the honest fix is to give cargo
/// somewhere to write.
fn writable_copy(dir: &std::path::Path, name: &str, version: &str) -> PathBuf {
    let src = store_root(name, version);
    let dst = dir.join(format!("{name}-{version}"));
    copy_tree(&src, &dst);
    dst
}

fn copy_tree(src: &std::path::Path, dst: &std::path::Path) {
    std::fs::create_dir_all(dst).expect("create dest dir");
    for entry in std::fs::read_dir(src).expect("read corpus dir") {
        let entry = entry.expect("corpus dir entry");
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_tree(&from, &to);
        } else {
            std::fs::copy(&from, &to).expect("copy corpus file");
            // Store files are read-only; clear that so cargo can rewrite them.
            let mut perms = std::fs::metadata(&to).expect("stat copy").permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            std::fs::set_permissions(&to, perms).expect("chmod copy");
        }
    }
}

fn corpus_available() -> bool {
    PACKAGES
        .iter()
        .all(|(n, v)| store_root(n, v).join("Cargo.toml").is_file())
}

fn dump_dir() -> PathBuf {
    PathBuf::from(std::env::var("NUDOX_DUMP_DIR").expect("set NUDOX_DUMP_DIR"))
}

/// Write one rendered response and echo its size, so a run is self-describing
/// even without the out-of-band tokenizer pass.
fn emit(case: &str, body: &str) {
    let dir = dump_dir();
    std::fs::create_dir_all(&dir).expect("create dump dir");
    let path = dir.join(format!("{case}.md"));
    std::fs::write(&path, body).expect("write dump");
    println!(
        "dump case={case} bytes={} path={}",
        body.len(),
        path.display()
    );
}

/// Returns the tools plus the tempdir guard, which must outlive them: dropping
/// it deletes the sources the producer is reading.
fn tools() -> (NudoxTools, tempfile::TempDir) {
    let scratch = tempfile::tempdir().expect("create writable corpus scratch");
    let engine = Engine::start_with_versions(
        EngineConfig::default(),
        PACKAGES
            .iter()
            .map(|(name, version)| PackageHistorySpec {
                name: (*name).to_owned(),
                language: ProducerLanguage::Rust,
                versions: vec![PackageVersionSpec {
                    root: writable_copy(scratch.path(), name, version),
                    version: (*version).to_owned(),
                }],
            })
            .collect(),
    );
    (NudoxTools::new(engine), scratch)
}

/// Block until every package has landed, failing loudly on a producer error
/// rather than hanging to the deadline.
async fn wait_for_packages(tools: &NudoxTools) {
    let rx = tools.engine().packages();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
    let mut seen = 0usize;
    while seen < PACKAGES.len() {
        match tokio::time::timeout_at(deadline, rx.recv_async()).await {
            Ok(Ok(PackageLoadEvent::Loaded { name, .. })) => {
                if PACKAGES.iter().any(|(n, _)| SharedStr::from(*n) == name) {
                    seen += 1;
                }
            }
            Ok(Ok(PackageLoadEvent::LoadFailed { name, error, .. })) => {
                panic!("{name} failed to load: {error}");
            }
            Ok(Ok(_)) => continue,
            Ok(Err(_)) => break,
            Err(_) => panic!("packages did not load within 300s"),
        }
    }
}

#[test]
#[ignore = "drives the real Rust producer over two real cargo workspaces"]
fn dump_every_tool_response() {
    if !corpus_available() {
        panic!("corpus missing; run `nix build .#checks.corpus`");
    }
    let runtime = Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let (tools, _scratch) = tools();
        wait_for_packages(&tools).await;

        // --- search: the highest-volume response, and the one most dominated
        //     by key characters ------------------------------------------------
        for (case, query, limit) in [
            ("search_deserialize_20", "deserialize", 20u32),
            ("search_deserialize_50", "deserialize", 50),
            ("search_memchr_20", "memchr", 20),
        ] {
            let result = tools
                .do_search(SearchSymbolsArgs {
                    query: query.to_owned(),
                    kinds: None,
                    packages: None,
                    limit: Some(limit),
                    cursor: None,
                })
                .await
                .expect("search must succeed");
            emit(case, &render_markdown(&result));

            // Reuse the hits as input to the reader and reference tools, so the
            // batch case is a realistic follow-up rather than a synthetic one.
            if case == "search_deserialize_20" {
                let keys: Vec<SymbolKeyDto> = result
                    .hits
                    .iter()
                    .take(8)
                    .map(|h| SymbolKeyDto::from_wire(&h.hit.key))
                    .collect();
                if !keys.is_empty() {
                    for (sub, format) in [
                        ("signature", SymbolFormat::Signature),
                        ("source", SymbolFormat::Source),
                    ] {
                        let batch = tools
                            .do_get_symbols(GetSymbolsArgs {
                                keys: keys.clone(),
                                format,
                            })
                            .await
                            .expect("get_symbols must succeed");
                        emit(&format!("get_symbols_8_{sub}"), &render_markdown(&batch));
                    }
                    let usages = tools
                        .do_find_usages(FindUsagesArgs {
                            key: keys[0].clone(),
                            limit: Some(30),
                            cursor: None,
                        })
                        .await
                        .expect("find_usages must succeed");
                    emit("find_usages_30", &render_markdown(&usages));
                }
            }
        }

        // --- discovery ------------------------------------------------------
        let packages = tools.do_list_packages().await.expect("list_packages");
        emit("list_packages", &render_markdown(&packages));

        let versions = tools
            .do_list_versions(ListVersionsArgs {
                package: PackageLineageDto("cargo:memchr".to_owned()),
            })
            .await
            .expect("list_versions");
        emit("list_versions", &render_markdown(&versions));

        // --- the graph escape hatch, including the schema payload -----------
        let query = tools
            .do_graph_query(GraphQueryArgs {
                query:
                    r"query { Symbols { key @output name @output kind @output signature @output } }"
                        .to_owned(),
                args: None,
                limit: Some(20),
                cursor: None,
            })
            .await
            .expect("graph_query");
        emit("graph_query_20", &render_markdown(&query));

        // Render the *tool's default* response, not the raw constant, so the
        // measurement includes the Markdown wrapper an agent actually receives.
        // The baseline worktree emits the full SDL here; this tree emits the
        // compact card. Both go through `MarkdownResult`, so the two dumps are
        // directly comparable.
        emit(
            "graph_schema",
            &render_markdown(&nudox_engine::mcp::tools::SchemaResult {
                schema: nudox_engine::mcp::SCHEMA_CARD.to_owned(),
                full: false,
            }),
        );
    });
}
