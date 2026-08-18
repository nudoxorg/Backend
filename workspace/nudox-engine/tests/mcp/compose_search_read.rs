//! `search` → `read` must compose (docs/MCP-SURFACE-PLAN.md §4, §5.1).
//!
//! # The defect these tests pin
//!
//! `search` returns each hit with **two** identifiers: the canonical
//! `ecosystem:name#introhex` `key`, and the readable `address`
//! (`npm:agent::'agent-loop'.runLoop`). `SearchResult`'s own doc comment says
//! of them: *"pass either it or `address` straight to `read` or `refs`"*, and
//! `read`'s MCP tool description says *"Read 1-32 symbols (keys or
//! addresses)"*.
//!
//! `refs` honours that. `read` does not. [`NudoxTools::do_get_symbols`]
//! pre-flight-validates every element with `SymbolKeyDto::to_wire`, which is a
//! *format* check for the legacy spelling only — it rejects an address before
//! the per-symbol path (`do_get_symbol` → `resolve_key_or_address`) that
//! already knows how to resolve one is ever reached.
//!
//! So the agent-visible consequence is not "a tool is missing a feature", it
//! is that the surface documents a composition it does not have: an agent that
//! reads the schema, searches, and pipes the address it was handed into `read`
//! gets an error naming a format it was never shown. Recovering costs a detour
//! through `graph_query` to fetch the key the search result was already
//! holding.
//!
//! # Why this is one file and not an assertion inside `tool_integration.rs`
//!
//! The property under test is a *relationship between two tools*, not the
//! behaviour of either. `tool_integration.rs` is organised one section per
//! tool, and a cross-tool invariant filed under whichever tool happens to come
//! first is exactly the kind of test that gets deleted with its section during
//! a redesign.

use nudox_engine::mcp::tools::{
    ReadArgs, RefsArgs, RefsDirection, SearchSymbolsArgs, SymbolFormat,
};
use nudox_engine::mcp::{NudoxTools, SymbolKeyDto};
use nudox_engine::{Engine, EngineConfig, PackageLoadEvent};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Harness
//
// Same shape as `tests/mcp/tool_integration.rs`'s: each integration test
// binary compiles separately, so there is no `tests/common` to share (that
// file's own note).
// ---------------------------------------------------------------------------

fn make_tools() -> NudoxTools {
    let engine = Engine::start_with_fixtures(EngineConfig::default());
    NudoxTools::new(engine)
}

async fn wait_for_corpus(tools: &NudoxTools) {
    let rx = tools.engine().packages();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match tokio::time::timeout_at(deadline, rx.recv_async()).await {
            Ok(Ok(PackageLoadEvent::Loaded { .. })) => return,
            Ok(Ok(_)) => continue,
            Ok(Err(_)) => return, // channel closed → already loaded
            Err(_) => panic!("corpus never seeded within 5 s"),
        }
    }
}

/// One search hit that carries both identifiers, as an agent would hold it.
struct Hit {
    key: SymbolKeyDto,
    address: String,
    display_name: String,
}

/// Search the fixture corpus and return the first hit the resolver gave an
/// address to.
///
/// Panics rather than skipping when no hit has one: a corpus in which `search`
/// renders no addresses at all would make every assertion below vacuously
/// true, which is the failure mode this whole file exists to prevent.
async fn first_addressable_hit(tools: &NudoxTools, query: &str) -> Hit {
    let result = tools
        .do_search(SearchSymbolsArgs {
            query: query.to_owned(),
            kinds: None,
            packages: None,
            limit: Some(50),
            cursor: None,
        })
        .await
        .expect("search over the fixture corpus succeeds");

    assert!(
        !result.hits.is_empty(),
        "query {query:?} must match something in the fixture corpus",
    );

    let hit = result
        .hits
        .iter()
        .find(|h| h.address.is_some())
        .unwrap_or_else(|| {
            panic!(
                "no hit for {query:?} carried an address; \
                 search returned {} hits, none addressable — \
                 the composition under test would be untested, not passing",
                result.hits.len(),
            )
        });

    Hit {
        key: SymbolKeyDto::from_wire(&hit.hit.key),
        address: hit
            .address
            .clone()
            .expect("filtered on address.is_some() above"),
        display_name: hit.hit.display_name.to_string(),
    }
}

// ---------------------------------------------------------------------------
// The composition
// ---------------------------------------------------------------------------

/// The whole point: the address `search` handed back must open in `read`.
///
/// Asserted against the *same symbol's* canonical key rather than merely
/// "returns Ok", because an address that silently resolved to a different
/// declaration would pass a smoke test and be worse than the error it
/// replaced.
#[tokio::test]
async fn read_opens_the_address_that_search_returned() {
    let tools = make_tools();
    wait_for_corpus(&tools).await;
    let hit = first_addressable_hit(&tools, "Point").await;

    let result = tools
        .do_read(ReadArgs {
            keys: vec![SymbolKeyDto(hit.address.clone())].into(),
            format: SymbolFormat::Signature,
        })
        .await
        .unwrap_or_else(|e| {
            panic!(
                "read rejected the address search emitted for {}: {e}\n  \
                 address: {}\n  \
                 key:     {}\n\
                 `SearchResult`'s doc comment says an agent may \
                 \"pass either it or `address` straight to `read` or `refs`\".",
                hit.display_name, hit.address, hit.key.0,
            )
        });

    assert_eq!(
        result.symbols.len(),
        1,
        "one address in must be one symbol out",
    );
    assert_eq!(
        result.symbols[0].key, hit.key,
        "the address must resolve to the very declaration search matched, \
         not merely to something readable",
    );
}

/// Reading by address and reading by key must produce the *same record*.
///
/// A weaker test — "both return Ok" — would still pass if the address path
/// resolved to a same-named declaration in another module, which is precisely
/// the confusion the `#hash` suffix in the address grammar exists to prevent.
#[tokio::test]
async fn the_address_and_the_key_read_identically() {
    let tools = make_tools();
    wait_for_corpus(&tools).await;
    let hit = first_addressable_hit(&tools, "Point").await;

    let by_key = tools
        .do_read(ReadArgs {
            keys: vec![hit.key.clone()].into(),
            format: SymbolFormat::Source,
        })
        .await
        .expect("reading by canonical key is the path that already works");

    let by_address = tools
        .do_read(ReadArgs {
            keys: vec![SymbolKeyDto(hit.address.clone())].into(),
            format: SymbolFormat::Source,
        })
        .await
        .unwrap_or_else(|e| panic!("read rejected address {}: {e}", hit.address));

    assert_eq!(
        by_key.symbols, by_address.symbols,
        "the two spellings of one identity must read as one record",
    );
}

/// A batch may mix the two spellings.
///
/// This is the realistic shape, not a contrived one: an agent accumulates
/// rows from several tools — `search` hands it addresses, a `graph_query`
/// hands it keys — and then reads them in one round trip. Rejecting the batch
/// because one element is spelled the other way turns a 1-call read into an
/// N-call one.
#[tokio::test]
async fn a_batch_may_mix_keys_and_addresses() {
    let tools = make_tools();
    wait_for_corpus(&tools).await;
    let hit = first_addressable_hit(&tools, "Point").await;

    let result = tools
        .do_read(ReadArgs {
            keys: vec![hit.key.clone(), SymbolKeyDto(hit.address.clone())].into(),
            format: SymbolFormat::Signature,
        })
        .await
        .unwrap_or_else(|e| panic!("a mixed batch was rejected: {e}"));

    assert_eq!(
        result.symbols.len(),
        2,
        "`read` preserves input order and arity, including duplicates",
    );
    assert_eq!(
        result.symbols[0], result.symbols[1],
        "the same declaration named two ways must read identically in one batch",
    );
}

/// `refs` already accepts the address. Pinned here so the fix to `read`
/// converges on the behaviour that exists rather than inventing a third one.
#[tokio::test]
async fn refs_accepts_the_same_address_read_must() {
    let tools = make_tools();
    wait_for_corpus(&tools).await;
    let hit = first_addressable_hit(&tools, "Point").await;

    tools
        .do_refs(RefsArgs {
            key: SymbolKeyDto(hit.address.clone()),
            direction: RefsDirection::In,
            limit: Some(10),
            cursor: None,
        })
        .await
        .unwrap_or_else(|e| {
            panic!(
                "refs is the tool that already resolves addresses; \
                 if this fails the address itself is malformed, not `read`: {e}",
            )
        });
}

// ---------------------------------------------------------------------------
// Failure reporting
// ---------------------------------------------------------------------------

/// An address that resolves to nothing must fail as an *address*.
///
/// The reason this is in the same file: once `read` accepts addresses, the
/// cheapest possible implementation — fall back to "invalid key" on anything
/// `to_wire` rejects — would make every unresolvable address report the
/// legacy-format error again, which is the original defect wearing a different
/// hat. `McpError::AddressUnresolved` carries the candidates and near-misses;
/// `InvalidArgument` carries a format lecture.
#[tokio::test]
async fn an_unresolvable_address_reports_why_not_a_format_complaint() {
    let tools = make_tools();
    wait_for_corpus(&tools).await;

    let error = tools
        .do_read(ReadArgs {
            keys: vec![SymbolKeyDto(
                "fixture:nudox-fixture-rich::no::such::declaration".to_owned(),
            )]
            .into(),
            format: SymbolFormat::Signature,
        })
        .await
        .expect_err("an address naming nothing must not succeed");

    let rendered = error.to_string();
    assert!(
        !rendered.contains("introhex"),
        "a well-formed but unresolvable address must not be reported as a \
         malformed key — the agent would go fix the spelling of something \
         that was spelled correctly. got: {rendered}",
    );
}
