//! An unfiltered `search` must return API, not parameter names.
//!
//! # The defect
//!
//! Parameters and fields are indexed as first-class symbols. That is the right
//! call for the index — `refs` on a parameter is a real question, and the
//! Trustfall schema exposes them as their own vertices — but it means the
//! symbol count of a package is dominated by them: a 12.6k-line TypeScript
//! package reports 3,039 symbols, most of which are `Param`.
//!
//! `search` ranks with `score = text_relevance × visibility_weight ×
//! kind_weight` (`workspace/nudox-engine/src/search/mod.rs`), and `kind_weight`
//! (`search/hits.rs:163`) already discounts members: `Param` 0.7, `Field` /
//! `Variant` 0.85, everything top-level 1.0. But a *multiplicative* discount
//! only reorders hits of comparable text relevance. An exact-name match on a
//! parameter scores 0.7 and still outranks a prefix match on the function that
//! declares it, so the first page of an exploratory query is parameter names.
//!
//! The `kinds` argument has existed the whole time. The problem is that it is
//! not the default: the caller must already know that members exist, that they
//! outnumber declarations, and what the twelve kind labels are spelled like,
//! before their first query returns anything usable. That is a discoverability
//! failure, and it lands hardest on exactly the case the tool is for — reading
//! a package you have never seen.
//!
//! # The shape of the fix these tests pin
//!
//! Default the *scope*, not the ranking: with no `kinds` argument, search
//! top-level declarations. Keep every existing explicit behaviour — naming
//! `Param` still searches parameters — and say on the response that members
//! were held back, so the default is discoverable rather than merely quiet.
//!
//! Ranking alone is not sufficient and this file is written to prove it: a
//! reweighting that pushed params below functions would still return a page of
//! params for a query that matches nothing else, and would still make
//! `symbol_count` unreadable. A default scope is a different claim from a
//! better score.

use std::fs;
use std::time::Duration;

use nudox_engine::mcp::tools::SearchSymbolsArgs;
use nudox_engine::mcp::NudoxTools;
use nudox_engine::{Engine, EngineConfig, PackageLoadEvent, PackageSpec, ProducerLanguage};

const PACKAGE: &str = "nudox_search_noise_fixture";

/// Everything here is named so that the query `connection` is a genuine
/// **prefix** of both the declarations and the members.
///
/// That detail is load-bearing. The name index matches exact and prefix, not
/// substring, so a fixture whose functions were named `open_connection` would
/// put them out of reach of a `connection` query entirely — and an earlier
/// draft of this file did exactly that. It passed anyway, because the defect
/// under test (a defaulted `kinds` list switching on the kind-facet section)
/// was dumping every declaration in the package regardless of the query. The
/// test was green *because of* the bug it was written to catch.
///
/// With these names the page has to be produced by real name matching, and
/// members outnumber declarations four to four — the shape a real package has,
/// where the ratio is far worse.
const LIB_RS: &str = r#"
/// A connection to something.
pub struct Connection {
    pub connection_id: u32,
    pub connection_name: String,
}

pub fn connection_open(connection_id: u32, connection_name: &str) -> Connection {
    Connection {
        connection_id,
        connection_name: connection_name.to_owned(),
    }
}

pub fn connection_close(connection_owned: Connection, connection_reason: &str) -> u32 {
    let _ = connection_reason;
    connection_owned.connection_id
}

pub fn connection_reset(connection_ref: &Connection, connection_force: bool) -> bool {
    let _ = connection_ref;
    connection_force
}
"#;

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("create temporary Rust package");
    fs::create_dir_all(dir.path().join("src")).expect("create src/");
    fs::write(
        dir.path().join("Cargo.toml"),
        format!("[package]\nname = \"{PACKAGE}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n"),
    )
    .expect("write manifest");
    fs::write(dir.path().join("src/lib.rs"), LIB_RS).expect("write source");
    dir
}

fn start(dir: &tempfile::TempDir) -> NudoxTools {
    let engine = Engine::start_with_producer(
        EngineConfig::default(),
        vec![PackageSpec {
            root: dir.path().to_path_buf(),
            name: PACKAGE.to_owned(),
            version: "0.1.0".to_owned(),
            language: ProducerLanguage::Rust,
        }],
    );
    NudoxTools::new(engine)
}

async fn wait_until_loaded(tools: &NudoxTools) {
    let events = tools.engine().packages();
    match tokio::time::timeout(Duration::from_mins(1), events.recv_async()).await {
        Ok(Ok(PackageLoadEvent::Loaded { .. })) => {}
        Ok(Ok(PackageLoadEvent::LoadFailed { error, .. })) => {
            panic!("the fixture package must load: {error}")
        }
        Ok(Ok(other)) => panic!("unexpected package event: {other:?}"),
        Ok(Err(error)) => panic!("package channel closed: {error}"),
        Err(elapsed) => panic!("engine did not load the fixture within 60 seconds: {elapsed:?}"),
    }
}

async fn search(tools: &NudoxTools, query: &str, kinds: Option<Vec<&str>>) -> Vec<(String, String)> {
    let result = tools
        .do_search(SearchSymbolsArgs {
            query: query.to_owned(),
            kinds: kinds.map(|k| k.into_iter().map(str::to_owned).collect()),
            packages: Some(vec![format!("cargo:{PACKAGE}")]),
            limit: Some(50),
            cursor: None,
        })
        .await
        .unwrap_or_else(|error| panic!("search for {query:?} must succeed: {error}"));
    result
        .hits
        .iter()
        .map(|h| (h.hit.display_name.to_string(), format!("{:?}", h.hit.kind)))
        .collect()
}

fn is_param(kind: &str) -> bool {
    kind.contains("Param")
}

// ---------------------------------------------------------------------------

/// The default page must be readable without knowing the kind vocabulary.
#[tokio::test]
async fn an_unfiltered_search_does_not_return_parameters() {
    let dir = fixture();
    let tools = start(&dir);
    wait_until_loaded(&tools).await;

    let hits = search(&tools, "connection", None).await;
    assert!(
        !hits.is_empty(),
        "the query must match the package's declarations",
    );

    let params: Vec<_> = hits.iter().filter(|(_, k)| is_param(k)).collect();
    assert!(
        params.is_empty(),
        "an unfiltered search returned {} parameter(s) out of {} hits. \
         Parameters are not an API surface a reader browses; they crowd out \
         the declarations that are. got: {hits:?}",
        params.len(),
        hits.len(),
    );
}

/// The declarations must actually be there — the default must *narrow*, not
/// empty, the page.
///
/// A fix that returned nothing for an unfiltered query would satisfy the test
/// above and be far worse than the defect.
#[tokio::test]
async fn the_declarations_survive_the_default() {
    let dir = fixture();
    let tools = start(&dir);
    wait_until_loaded(&tools).await;

    let hits = search(&tools, "connection", None).await;
    let names: Vec<&str> = hits.iter().map(|(n, _)| n.as_str()).collect();

    for expected in ["Connection", "connection_open", "connection_close"] {
        assert!(
            names.contains(&expected),
            "{expected} is top-level API and must be on the default page: \
             {names:?}",
        );
    }
}

/// An explicit ask is still honoured.
///
/// The default narrows the *unspecified* case only. A caller who names `Param`
/// is answering a different question — "where is this parameter used" — and
/// must keep getting parameters.
#[tokio::test]
async fn naming_param_explicitly_still_searches_parameters() {
    let dir = fixture();
    let tools = start(&dir);
    wait_until_loaded(&tools).await;

    let hits = search(&tools, "connection", Some(vec!["Param"])).await;
    assert!(
        !hits.is_empty(),
        "asking for Param by name must return parameters — the default scope \
         must not become a filter the caller cannot turn off",
    );
    assert!(
        hits.iter().all(|(_, k)| is_param(k)),
        "an explicit kind filter must still restrict to exactly that kind: \
         {hits:?}",
    );
}

/// Fields behave the same way as parameters, and for the same reason.
#[tokio::test]
async fn fields_are_reachable_when_named() {
    let dir = fixture();
    let tools = start(&dir);
    wait_until_loaded(&tools).await;

    let hits = search(&tools, "connection", Some(vec!["Field"])).await;
    assert!(
        !hits.is_empty(),
        "`Connection` has two fields whose names contain the query; naming \
         Field must find them",
    );
}

/// The default must be discoverable from the response.
///
/// A quiet default is a second thing the reader has to already know. The
/// response must say that members were held back, so the way to see them is
/// learnable from a result rather than only from the schema.
#[tokio::test]
async fn the_response_says_members_were_held_back() {
    let dir = fixture();
    let tools = start(&dir);
    wait_until_loaded(&tools).await;

    let result = tools
        .do_search(SearchSymbolsArgs {
            query: "connection".to_owned(),
            kinds: None,
            packages: Some(vec![format!("cargo:{PACKAGE}")]),
            limit: Some(50),
            cursor: None,
        })
        .await
        .expect("search must succeed");

    let note = format!("{:?}", result.excluded_kinds);
    assert!(
        result.excluded_kinds.is_some(),
        "an unfiltered search that narrowed its own scope must say so; \
         otherwise a reader who wants a parameter concludes it is not indexed",
    );
    assert!(
        note.contains("Param"),
        "the note must name what was held back, so the reader knows what to \
         pass to `kinds`: got {note}",
    );
}

/// ...and must be absent when it would be noise.
///
/// Same discipline as `SearchResult::semantic`: present only when the answer
/// is not the straightforward one, so it costs nothing in the common case.
#[tokio::test]
async fn an_explicit_filter_carries_no_exclusion_note() {
    let dir = fixture();
    let tools = start(&dir);
    wait_until_loaded(&tools).await;

    let result = tools
        .do_search(SearchSymbolsArgs {
            query: "connection".to_owned(),
            kinds: Some(vec!["Function".to_owned()]),
            packages: Some(vec![format!("cargo:{PACKAGE}")]),
            limit: Some(50),
            cursor: None,
        })
        .await
        .expect("search must succeed");

    assert!(
        result.excluded_kinds.is_none(),
        "the caller chose the scope; there is nothing to disclose: {:?}",
        result.excluded_kinds,
    );
}
