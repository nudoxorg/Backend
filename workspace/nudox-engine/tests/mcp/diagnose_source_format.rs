//! Guards MCP-SURFACE-PLAN.md §9.2's `get_symbols`/`read` `format` finding:
//! against real serde 1.0.196, the top 8 `search("deserialize")` hits are all
//! declarative-macro-generated `Deserialize` impls for built-in types (serde's
//! `de::impls` module macro-generates them for `Mutex<T>`, `NonZero<u8>`,
//! arrays, `RefCell<T>`, `i64`, `Box<T>`, ...). The Rust producer's
//! `fn_location` (`workspace/compiler/languages/src/rust/ra/source.rs`) tries
//! `Semantics::original_range_opt` to map a macro-expanded item back to its
//! call site, but that mapping does not resolve through this declarative-macro
//! shape, so these land as `SourceLocation::Unlocated(MacroExpanded)`: no file
//! span at all, not merely a read failure. `source_excerpts`
//! (`workspace/compiler/languages/src/lib.rs`) correctly skips entries with no
//! span *without* recording a `SourceExcerptIssue` — this is an honest
//! "there is nothing to point at", a different and larger category than
//! "materialization failed" — so no diagnostic is ever logged for it either.
//!
//! Net effect: `read`/`get_symbols` with `format=source` silently produces the
//! same text as `format=signature` for any declaration in this class, with
//! nothing in the response to tell an agent which branch it got. This is not a
//! bug to paper over with fabricated source text (there is no single faithful
//! "this declaration's own text" for a macro-expanded item — see
//! `Unlocated::MacroExpanded`'s doc comment) — it is a real limitation, and
//! `SymbolFormat::Source`'s doc comment and the `read` tool description
//! (`workspace/nudox-engine/src/mcp/server.rs`) now say so. This test pins the
//! underlying behaviour so that if a future rust-analyzer call-site mapping
//! pass closes the gap, this test fails loudly and someone remembers to loosen
//! those descriptions back down rather than leaving a stale disclaimer.
//!
//! Run: `cargo test -p nudox-engine --test mcp_diagnose_source_format -- --ignored --nocapture`

use std::path::PathBuf;
use std::time::Duration;

use nudox_engine::mcp::tools::{GetSymbolsArgs, SearchSymbolsArgs, SymbolFormat};
use nudox_engine::mcp::{NudoxTools, SymbolKeyDto};
use nudox_engine::{
    Engine, EngineConfig, PackageHistorySpec, PackageLoadEvent, PackageVersionSpec,
    ProducerLanguage, SharedStr,
};
use tokio::runtime::Runtime;

const PACKAGES: [(&str, &str); 1] = [("serde", "1.0.196")];

fn store_root(name: &str, version: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../result/{name}-{version}"))
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("/nonexistent"))
}

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

#[test]
#[ignore = "drives the real Rust producer over a real cargo workspace"]
fn format_source_silently_degrades_to_signature_for_macro_generated_impls() {
    assert!(corpus_available(), "corpus missing; run `nix build .#checks.corpus`");
    let runtime = Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
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
        let tools = NudoxTools::new(engine);

        let rx = tools.engine().packages();
        let deadline = tokio::time::Instant::now() + Duration::from_mins(5);
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
                Ok(Ok(_)) => {},
                Ok(Err(_)) => break,
                Err(elapsed) => panic!("packages did not load within 300s: {elapsed:?}"),
            }
        }

        let result = tools
            .do_search(SearchSymbolsArgs {
                query: "deserialize".to_owned(),
                kinds: None,
                packages: None,
                limit: Some(20),
                cursor: None,
            })
            .await
            .expect("search must succeed");

        let keys: Vec<SymbolKeyDto> =
            result.hits.iter().take(8).map(|h| SymbolKeyDto::from_wire(&h.hit.key)).collect();
        assert_eq!(keys.len(), 8, "expected the real serde corpus to yield 8 hits for 'deserialize'");

        let source_batch = tools
            .do_get_symbols(GetSymbolsArgs { keys: keys.clone(), format: SymbolFormat::Source })
            .await
            .expect("get_symbols(source) must succeed");
        let sig_batch = tools
            .do_get_symbols(GetSymbolsArgs { keys: keys.clone(), format: SymbolFormat::Signature })
            .await
            .expect("get_symbols(signature) must succeed");

        for (i, (s, g)) in source_batch.symbols.iter().zip(sig_batch.symbols.iter()).enumerate() {
            assert!(
                s.location.is_none(),
                "[{i}] {} unexpectedly got a location ({:?}) — if the rust producer's macro call-site \
mapping now resolves this shape, `format=source` may no longer silently degrade here; update this \
test, `SymbolFormat::Source`'s doc comment, and the `read` tool description in server.rs together",
                s.path,
                s.location,
            );
            assert!(
                s.source.is_none(),
                "[{i}] {} unexpectedly has exact source text ({:?}) despite no recorded location",
                s.path,
                s.source,
            );
            assert_eq!(
                s.signature, g.signature,
                "[{i}] {} — format=source and format=signature should render identically here \
(the documented silent-degradation case); if they now differ, the underlying gap may have closed",
                s.path,
            );
        }
    });
}
