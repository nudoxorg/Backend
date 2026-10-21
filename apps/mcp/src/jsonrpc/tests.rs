//! MCP surface tests.
//!
//! These cases assert *content*, never counts: the tool a registry row is
//! reachable as, the Markdown an agent reads, the exact coordinate it passes
//! back, the slug and the next tool call a refusal carries. A fixture engine
//! stands in for the daemon, so every assertion here is about what this surface
//! decides rather than about what a live index happens to hold.

#![allow(clippy::expect_used, clippy::naive_bytecount, clippy::panic)]

use super::tools::QUERY_TOOL;
use super::*;
use backend_library::{
    Basis, COMMANDS, CommandReply, CompileExecutionIntent, Coverage, DeclarationKind, Document,
    Fragment, Freshness, Frontier, IndexCancelReceipt, IndexCancelStatus, IndexJobObservation,
    IndexJobOutcome, IndexJobProgressEvent, IndexJobProgressKind, IndexJobStage, IndexJobTerminal,
    IndexJobTicket, IndexProgressPage, IndexSearchCursor, IndexSearchPage, IndexSearchResultCount,
    IndexStartResult, Intent, Lane, PackageReference,
    ProjectionPage, Reason, Row, RowId, SourceAvailability, SourceExcerpt, SourceExcerptExtent,
    SourceLocation, SurfaceReply, ViewRoot, ViewSnapshot, object_version, package_key, symbol_key,
    view_key, view_state_root,
};
use backend_present::{domain_name, encode_answer, grammar_for, grammar_for_tool};

#[cfg(unix)]
#[path = "tests/index_overlap.rs"]
mod index_overlap;
#[path = "tests/initial_owner.rs"]
mod initial_owner;
#[path = "tests/search_admission.rs"]
mod search_admission;
#[path = "tests/named_call.rs"]
mod named_call;
#[path = "tests/package_compiler_outcomes.rs"]
mod package_compiler_outcomes;
#[path = "tests/package_graph_budget.rs"]
mod package_graph_budget;
#[path = "tests/record_budget_pages.rs"]
mod record_budget_pages;
#[path = "tests/document_section_pages.rs"]
mod document_section_pages;
#[path = "tests/semantic_selection.rs"]
mod semantic_selection;
#[path = "tests/semantic_source_pages.rs"]
mod semantic_source_pages;
#[path = "tests/source_membership_pages.rs"]
mod source_membership_pages;
#[path = "tests/surface_fidelity.rs"]
mod surface_fidelity;
#[path = "tests/token_outline.rs"]
mod token_outline;
#[path = "tests/typescript_admission.rs"]
mod typescript_admission;

const PROJECT: &str = "/abs/polyglot";
const MODULE: &str = "/abs/polyglot::src/lib.rs";
const DECLARATION: &str = "/abs/polyglot::src/lib.rs:2::ferris";
const MISSING: &str = "/abs/polyglot::src/lib.rs:999::nothing";

// Canonical unpadded base64url JSON, matching the index-search issuer's
// encoding. The fixture product still owns its semantic state admission.
const FIXTURE_INDEX_CURSOR: &str = "eyJ2ZXJzaW9uIjo0LCJmaXh0dXJlIjoibWF2ZW4tb3duZXItdjQifQ";

fn fixture_page_cursor(continuation: PageContinuation) -> String {
    // Use the same typed query cursor bytes as Client::encode_page_continuation.
    format!(
        "pc1-{}",
        hex_bytes(continuation.cursor().encode_query().as_ref())
    )
}

/// Independent historical/vector encoder: do not call the production signer
/// or MAC helper, so the tests retain the exact old domain separation law.
fn cursor_wrapper_vector(
    version: &str,
    expiry: &str,
    payload: &str,
    key: [u8; 32],
    context: &[u8],
) -> String {
    let body = format!("{expiry}-{payload}");
    let authenticated = if version == "mcp1" {
        body.clone()
    } else {
        format!("{version}-{body}")
    };
    let mut bytes = authenticated.into_bytes();
    bytes.push(0);
    bytes.extend_from_slice(context);
    let hash = blake3::keyed_hash(&key, &bytes);
    format!("{version}-{body}-{}", hex_bytes(&hash.as_bytes()[..16]))
}

#[test]
fn direct_cursor_legacy_vectors_preserve_exact_mcp1_domain_and_encoding() {
    let server = Server::with_authority(Fake::default(), PROJECT.to_owned(), [7; 32]);
    for owner in [FIXTURE_INDEX_CURSOR, "historical UTF-8 λ\0payload"] {
        let encoded = URL_SAFE_NO_PAD.encode(owner.as_bytes());
        for expiry in ["18446744073709551615", "018446744073709551615"] {
            let token = cursor_wrapper_vector("mcp1", expiry, &encoded, [7; 32], b"legacy-context");
            assert_eq!(
                server
                    .verify_cursor_token(&token, b"legacy-context")
                    .expect("legacy verification")
                    .as_deref(),
                Some(owner)
            );
            assert!(
                server
                    .verify_cursor_token(&token, b"other-context")
                    .expect("wrong context")
                    .is_none()
            );
        }
        let padded = cursor_wrapper_vector(
            "mcp1",
            "18446744073709551615",
            &format!("{encoded}="),
            [7; 32],
            b"legacy-context",
        );
        assert!(
            server
                .verify_cursor_token(&padded, b"legacy-context")
                .expect("noncanonical legacy encoding")
                .is_none()
        );
    }
}

#[test]
fn direct_cursor_authenticates_version_expiry_payload_and_existing_context() {
    let server = Server::with_authority(Fake::default(), PROJECT.to_owned(), [7; 32]);
    for prefix in ["pc3-", "pc4-"] {
        let owner = format!("{prefix}{}", URL_SAFE_NO_PAD.encode([0xfb, 0xff, 0xfe]));
        assert!(
            owner.contains('-') && owner.contains('_'),
            "exercise owner separators"
        );
        assert!(
            owner.ends_with('-'),
            "MAC must split after a trailing owner separator"
        );
        let token = server
            .sign_cursor_token_at(u64::MAX, &owner, b"query-role-project")
            .expect("direct token");
        assert_eq!(
            token,
            cursor_wrapper_vector(
                "mcp2",
                &u64::MAX.to_string(),
                &owner,
                [7; 32],
                b"query-role-project"
            )
        );
        assert_eq!(
            server
                .verify_cursor_token(&token, b"query-role-project")
                .expect("direct verification")
                .as_deref(),
            Some(owner.as_str())
        );
        for changed in [
            token.replacen("mcp2-", "mcp1-", 1),
            token.replacen("mcp2-", "mcp3-", 1),
            token.replacen(&u64::MAX.to_string(), "18446744073709551614", 1),
            token.replacen(prefix, "pc2-", 1),
            token.replacen(&owner, &format!("{prefix}AA"), 1),
        ] {
            assert!(
                server
                    .verify_cursor_token(&changed, b"query-role-project")
                    .expect("tampered wrapper")
                    .is_none()
            );
        }
        for expiry in [
            "0",
            "018446744073709551615",
            "+18446744073709551615",
            "18446744073709551616",
        ] {
            let changed =
                cursor_wrapper_vector("mcp2", expiry, &owner, [7; 32], b"query-role-project");
            assert!(
                server
                    .verify_cursor_token(&changed, b"query-role-project")
                    .expect("closed expiry grammar")
                    .is_none()
            );
        }
        assert!(
            server
                .verify_cursor_token(&token, b"other-query-role-project")
                .expect("context mutation")
                .is_none()
        );
        let rotated = Server::with_authority(Fake::default(), PROJECT.to_owned(), [8; 32]);
        assert!(
            rotated
                .verify_cursor_token(&token, b"query-role-project")
                .expect("rotated key")
                .is_none()
        );
        let old = cursor_wrapper_vector(
            "mcp1",
            &u64::MAX.to_string(),
            &URL_SAFE_NO_PAD.encode(owner.as_bytes()),
            [7; 32],
            b"query-role-project",
        );
        assert_eq!(
            old.len() - token.len(),
            URL_SAFE_NO_PAD.encode(owner.as_bytes()).len() - owner.len(),
            "savings are exactly the removed outer encoding"
        );
    }
}

#[test]
fn direct_cursor_encoding_boundaries_do_not_mint_or_load_cold_authority() {
    let fixture = LazyAuthorityFixture::new();
    let authority = CursorAuthority::workspace(&fixture.paths);
    let server = Server::with_authority(Fake::default(), PROJECT.to_owned(), authority);
    for owner in [
        "",
        "plaintext",
        "pc1-short",
        "pc2-0",
        "pc2-AA",
        "pc3-",
        "pc3-Zg=",
        "pc3-Zh",
        "pc4-",
        "pc4-Zg=",
        "pc4-Zh",
        "pc4-λ",
        "pc4-AA\n",
        "is1-",
        "is1-Zg",
        "is1-Zg==",
        "pc5-AA",
        "pc3-λ",
        "pc3-AA\n",
        "{\"raw\":true}",
    ] {
        assert!(
            server
                .sign_cursor_token_at(u64::MAX, owner, b"query")
                .is_err()
        );
        let forged = cursor_wrapper_vector("mcp2", &u64::MAX.to_string(), owner, [7; 32], b"query");
        assert!(
            server
                .verify_cursor_token(&forged, b"query")
                .expect("cold grammar rejection")
                .is_none()
        );
        assert!(
            !fixture.paths.data().exists(),
            "invalid owner encoding must not provision state"
        );
    }
    let oversized = "A".repeat(backend_library::MAX_INDEX_SEARCH_CURSOR_BYTES + 1);
    assert!(
        server
            .sign_cursor_token_at(u64::MAX, &oversized, b"query")
            .is_err()
    );
    let forged = format!("mcp2-{}-{oversized}-{}", u64::MAX, "00".repeat(16));
    assert!(
        server
            .verify_cursor_token(&forged, b"query")
            .expect("raw size rejection")
            .is_none()
    );
    assert!(!fixture.paths.data().exists());
}

#[test]
fn direct_cursor_owner_encodings_and_exact_size_survive_authenticated_roundtrip() {
    let server = Server::with_authority(Fake::default(), PROJECT.to_owned(), [7; 32]);
    let max_body = format!(
        "{{\"x\":\"{}\"}}",
        "x".repeat(backend_library::MAX_INDEX_SEARCH_CURSOR_BYTES / 4 * 3 - 8)
    );
    let largest = URL_SAFE_NO_PAD.encode(max_body);
    assert_eq!(
        largest.len(),
        backend_library::MAX_INDEX_SEARCH_CURSOR_BYTES
    );
    let page = fixture_page_cursor(PageContinuation::from_cursor(backend_library::Cursor::new()));
    for owner in [
        page,
        "pc2-00ff".to_owned(),
        "pc3-AA".to_owned(),
        "pc4-AA".to_owned(),
        format!("pc5-{}", URL_SAFE_NO_PAD.encode(b"portable binary outline certificate")),
        compressed_index_search_owner(br#"{"state":"fixture"}"#),
        FIXTURE_INDEX_CURSOR.to_owned(),
        largest,
    ] {
        let token = server
            .sign_cursor_token_at(u64::MAX, &owner, b"same-context")
            .expect("admitted owner encoding");
        assert_eq!(
            server
                .verify_cursor_token(&token, b"same-context")
                .expect("authenticated exact owner")
                .as_deref(),
            Some(owner.as_str())
        );
    }
    for owner in [
        "pc3-Zh",
        "pc3-Zg=",
        "pc1-00",
        "pc2-0",
        "pc2-FF",
        "pc5-A",
        "pc5-YQ==",
        "pc5-",
        "cGxhaW50ZXh0",
        "pc3-AA\0",
    ] {
        let token = cursor_wrapper_vector(
            "mcp2",
            &u64::MAX.to_string(),
            owner,
            [7; 32],
            b"same-context",
        );
        assert!(
            server
                .verify_cursor_token(&token, b"same-context")
                .expect("authentic MAC cannot bypass inner grammar")
                .is_none()
        );
    }
}

#[test]
fn outline_pc5_is_routed_as_an_untrusted_owner_witness_only_after_outer_context_authentication() {
    let mut server = ready(Fake::default());
    let owner = format!(
        "pc5-{}",
        URL_SAFE_NO_PAD.encode(b"bounded binary outline certificate fixture")
    );
    let context_arguments = json!({"path":PROJECT,"limit":4,"detail":"full"});
    let context = continuation_context(
        PROJECT,
        "backend.outline",
        context_arguments.as_object().expect("arguments"),
        Detail::Full,
    );
    let token = server
        .sign_cursor_token_at(u64::MAX, &owner, &context)
        .expect("PC5 framing is signable");
    let reply = request(
        &mut server,
        "tools/call",
        &json!({"name":"backend.outline","arguments":{
            "path":PROJECT,"limit":4,"detail":"full","cursor":token
        }}),
    );
    assert!(
        reply.get("error").is_some() || reply["result"]["isError"] == true,
        "test product refusal is retained: {reply}"
    );
    assert_eq!(server.product.outline_owner_tokens, [owner]);
    assert!(server.product.page_windows.is_empty(), "PC5 never enters legacy typed-cursor decoding");

    let other_context = continuation_context(
        PROJECT,
        "backend.outline",
        json!({"path":"/abs/other","limit":4,"detail":"full"})
            .as_object()
            .expect("other arguments"),
        Detail::Full,
    );
    let rebound = server
        .sign_cursor_token_at(u64::MAX, &owner, &other_context)
        .expect("fixture can sign a deliberately wrong context");
    let reply = request(
        &mut server,
        "tools/call",
        &json!({"name":"backend.outline","arguments":{
            "path":PROJECT,"limit":4,"detail":"full","cursor":rebound
        }}),
    );
    assert!(reply.get("error").is_some(), "context rebind is rejected: {reply}");
    assert_eq!(server.product.outline_owner_tokens, [owner]);
}

fn compressed_index_search_owner(json: &[u8]) -> String {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    std::io::Write::write_all(&mut encoder, json).expect("JSON cursor bytes");
    format!(
        "is1-{}",
        URL_SAFE_NO_PAD.encode(encoder.finish().expect("complete zlib stream"))
    )
}

#[test]
fn compressed_index_search_framing_requires_bounded_complete_zlib_json_object() {
    assert!(valid_direct_cursor_owner(&compressed_index_search_owner(
        br#"{"state":"fixture"}"#
    )));
    assert!(!valid_direct_cursor_owner(&compressed_index_search_owner(b"[]")));
    assert!(!valid_direct_cursor_owner(&compressed_index_search_owner(b"not-json")));

    let valid = compressed_index_search_owner(br#"{"state":"fixture"}"#);
    let encoded = valid.strip_prefix("is1-").expect("prefix");
    let mut trailing = URL_SAFE_NO_PAD.decode(encoded).expect("canonical base64");
    trailing.push(0);
    assert!(!valid_direct_cursor_owner(&format!(
        "is1-{}",
        URL_SAFE_NO_PAD.encode(trailing)
    )));

    let mut corrupt = URL_SAFE_NO_PAD.decode(encoded).expect("canonical base64");
    *corrupt.last_mut().expect("checksum byte") ^= 1;
    assert!(!valid_direct_cursor_owner(&format!(
        "is1-{}",
        URL_SAFE_NO_PAD.encode(corrupt)
    )));

    assert!(!valid_direct_cursor_owner(&compressed_index_search_owner(
        &vec![b'x'; backend_library::MAX_INDEX_SEARCH_CURSOR_BYTES + 1]
    )));
}

#[test]
fn direct_cursor_legacy_named_request_context_survives_and_cannot_cross_roles() {
    // Exact historical BTreeMap serialization of the named-tool request.
    // This vector must not derive its context from the current helper.
    let context = br#"{"detail":"summary","limit":1,"query":"maven","schema":"mcp1","tool":"backend.index_search","workspace":"/abs/polyglot"}"#;
    let token = cursor_wrapper_vector(
        "mcp1",
        "18446744073709551615",
        &URL_SAFE_NO_PAD.encode(FIXTURE_INDEX_CURSOR),
        [9; 32],
        context,
    );
    let mut named = ready(Fake {
        surface_index_search_pages: true,
        ..Fake::default()
    });
    let resumed = call(
        &mut named,
        "backend.index_search",
        &json!({"query":"maven","limit":1,"detail":"summary","cursor":token}),
    );
    assert_eq!(resumed["isError"], false);
    assert_eq!(
        named.product.surface_index_search_seen,
        vec![Some(FIXTURE_INDEX_CURSOR.to_owned())]
    );
    let mut other_role = ready(Fake {
        surface_index_search_pages: true,
        ..Fake::default()
    });
    let refused = call(
        &mut other_role,
        SURFACE_TOOL,
        &json!({"command":{"operation":"index-search","query":"maven","limit":1,"cursor":token},"detail":"summary"}),
    );
    assert_eq!(refused["isError"], true);
    assert!(other_role.product.surface_index_search_seen.is_empty());
}

fn retained_go_binary_owner() -> String {
    let original = include_bytes!(
        "../../../../crates/library/wire/command/fixtures/current-go-graph-continuation.json"
    );
    // Keep the actual DTO27 capture unchanged. This is a derived current
    // command with the exact original claims, not a historical-peer replay.
    let mut current: Value = serde_json::from_slice(original).expect("retained producer envelope");
    assert_eq!(current["version"], 27);
    assert!(backend_library::decode_command_body(original).is_err());
    let claims = current["certificate"].clone();
    current["version"] = json!(backend_library::DTO_VERSION);
    assert_eq!(current["certificate"], claims);
    let request = backend_library::decode_command_body(
        &serde_json::to_vec(&current).expect("current version envelope"),
    )
    .expect("unchanged producer claims pass current strict admission");
    backend_library::encode_command_body(&request).expect("unchanged encoder admission");
    let binary =
        backend_library::encode_binary_certificate_command(&request).expect("binary proof");
    let expanded =
        backend_library::expand_binary_certificate_command(&binary).expect("exact expansion");
    assert_eq!(
        expanded,
        serde_json::to_vec(&request).expect("canonical current envelope")
    );
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut encoder, &binary).expect("bounded actual proof");
    format!(
        "pc4-{}",
        URL_SAFE_NO_PAD.encode(encoder.finish().expect("finish"))
    )
}

#[test]
fn actual_binary_owner_mac_restarts_and_refuses_cross_surface_query_detail_and_key() {
    let fixture = LazyAuthorityFixture::new();
    let owner = retained_go_binary_owner();
    let arguments =
        json!({"coordinate":"/work/sol61-cli-proof/corpora/gorilla-mux::mux.go:47::Router"});
    let arguments = arguments.as_object().expect("arguments");
    let context = continuation_context(PROJECT, "backend.graph", arguments, Detail::Summary);
    let first = Server::with_authority(
        Fake::default(),
        PROJECT.to_owned(),
        CursorAuthority::workspace(&fixture.paths),
    );
    let token = first
        .sign_cursor_token(&owner, &context)
        .expect("real signer");
    drop(first);
    let fresh = Server::with_authority(
        Fake::default(),
        PROJECT.to_owned(),
        CursorAuthority::workspace(&fixture.paths),
    );
    for _ in 0..2 {
        assert_eq!(
            fresh
                .verify_cursor_token(&token, &context)
                .expect("durable authority")
                .as_deref(),
            Some(owner.as_str()),
        );
    }
    let mut changed_coordinate = arguments.clone();
    changed_coordinate.insert("coordinate".to_owned(), json!("/other::Router"));
    let mut changed_limit = arguments.clone();
    changed_limit.insert("limit".to_owned(), json!(3));
    let mut changed_variables = arguments.clone();
    changed_variables.insert("variables".to_owned(), json!({"scope":"other"}));
    for other in [
        continuation_context(
            PROJECT,
            "backend.graph",
            &changed_coordinate,
            Detail::Summary,
        ),
        continuation_context(PROJECT, "backend.graph", &changed_limit, Detail::Summary),
        continuation_context(
            PROJECT,
            "backend.graph",
            &changed_variables,
            Detail::Summary,
        ),
        continuation_context(PROJECT, "backend.graph", arguments, Detail::Full),
        continuation_context(PROJECT, "backend.search", arguments, Detail::Summary),
        continuation_context(PROJECT, SURFACE_TOOL, arguments, Detail::Summary),
        continuation_context(
            "/other-workspace",
            "backend.graph",
            arguments,
            Detail::Summary,
        ),
    ] {
        assert!(
            fresh
                .verify_cursor_token(&token, &other)
                .expect("changed context")
                .is_none()
        );
    }
    let foreign = LazyAuthorityFixture::new();
    let foreign_server = Server::with_authority(
        Fake::default(),
        PROJECT.to_owned(),
        CursorAuthority::workspace(&foreign.paths),
    );
    assert!(
        foreign_server
            .verify_cursor_token(&token, &context)
            .expect("cold foreign authority")
            .is_none()
    );
    assert!(
        !foreign.paths.data().exists(),
        "verification never mints an authority"
    );
    let rotated = Server::with_authority(Fake::default(), PROJECT.to_owned(), [8; 32]);
    assert!(
        rotated
            .verify_cursor_token(&token, &context)
            .expect("rotated key")
            .is_none()
    );
    for changed in [
        token.replacen("pc4-", "pc3-", 1),
        token.replacen("mcp2-", "mcp1-", 1),
    ] {
        assert!(
            fresh
                .verify_cursor_token(&changed, &context)
                .expect("changed version")
                .is_none()
        );
    }
}

#[test]
fn actual_go_25_row_summary_retains_typed_facts_and_a_valid_binary_owner_mac() {
    // Egress fixture from actual Go 0066. It asserts preservation and signed
    // transport, not that a fake product admits the captured native owner.
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/current-go-graph-summary-input.json"))
            .expect("retained actual rows");
    let body = &fixture["body"];
    let typed: backend_present::RecordListDto =
        serde_json::from_value(body.clone()).expect("typed Summary");
    assert_eq!(typed.records.len(), 25);
    assert!(typed.more);
    let project = "/work/sol61-cli-proof/corpora/gorilla-mux";
    let arguments = json!({"coordinate":&typed.query});
    let context = continuation_context(
        project,
        "backend.graph",
        arguments.as_object().expect("args"),
        Detail::Summary,
    );
    let server = Server::with_authority(Fake::default(), project.to_owned(), [7; 32]);
    let owner = retained_go_binary_owner();
    let token = server
        .sign_cursor_token_at(u64::MAX, &owner, &context)
        .expect("production MAC");
    assert_eq!(
        server
            .verify_cursor_token(&token, &context)
            .expect("MAC admission")
            .as_deref(),
        Some(owner.as_str())
    );
    let payload = backend_present::encode_serializable(
        "records",
        Detail::Summary,
        Some(&token),
        body,
        DEFAULT_RESPONSE_BUDGET_BYTES,
    )
    .expect("complete actual page fits byte budget");
    let structured: Value = serde_json::from_slice(&payload.bytes).expect("typed envelope");
    let preview = format!(
        "{}\n25 row(s); complete records and native operands are in `structuredContent.records`.\nContinue `backend.graph` with the same arguments; copy `structuredContent.nextCursor` to `arguments.cursor`.",
        fixture["coverage_preview"]
            .as_str()
            .expect("actual coverage preview"),
    );
    let response = success(json!(45), tool_result(&preview, structured, false));
    let restored: backend_present::RecordListDto =
        serde_json::from_value(response["result"]["structuredContent"].clone())
            .expect("typed retained rows");
    assert_eq!(
        restored, typed,
        "all native operands, IDs, sites, facts, coverage and order"
    );
    assert_eq!(response["result"]["isError"], false);
    assert_context_bounded(&response);
    // Explicit opt-in evidence export lets the guarded native test supply a
    // genuinely signed packet to the pinned tokenizer oracle. No token count
    // is inferred from the byte estimate or claimed without that second check.
    if let Some(path) = std::env::var_os("NUDOX_BINARY_CURSOR_PACKET") {
        std::fs::write(path, serde_json::to_vec(&response).expect("exact packet"))
            .expect("requested token-audit artifact");
    }
}

// ---------------------------------------------------------------------------
// the fixture engine
// ---------------------------------------------------------------------------

/// A fixture product with one project, one module, and one declaration.
#[derive(Default)]
struct Fake {
    surface_dispatches: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
    health_calls: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
    /// When set, every probe reports the endpoint as unreachable.
    offline: bool,
    /// A registered project with no published declarations has no outline.
    empty_outline: bool,
    /// Keep genuine outline transport/proof failures distinct from absence.
    outline_error: Option<ClientError>,
    /// Records the explicit owner-aware PC5 route without treating the
    /// untrusted page witness as a locally admitted typed continuation.
    outline_owner_tokens: Vec<String>,
    /// Exact source availability returned by the document/source boundary.
    document_override: Option<Document>,
    /// Negative control: withhold an overridden document from the member projection.
    omit_document_override_member: bool,
    /// Optional graph rows used to exercise the complete-page admission cap.
    graph_rows: Option<Box<[GraphQueryRow]>>,
    /// Synthetic typed query membership under real transport admission.
    graph_paged_rows: Option<Box<[GraphQueryRow]>>,
    graph_query_source_changes: bool,
    /// Identity-free typed failed reply from the direct graph-page route.
    graph_failure: Option<backend_library::CommandFailure>,
    /// Typed failure from the generic graph query route.
    graph_query_error: Option<ClientError>,
    /// Make the graph fixture return one authenticated continuation page.
    graph_continue: bool,
    /// Owner-issued continuation retained by the fixture encoder.
    next_continuation: Option<PageContinuation>,
    /// Make an otherwise valid owner cursor stale at decode time.
    stale_cursor: bool,
    /// Inject a typed decode failure only after an actual catalog cursor was issued.
    continuation_error: Option<ClientError>,
    /// Optional product reply used by the high-fanout surface budget case.
    surface_reply: Option<SurfaceReply>,
    /// Retains the active durable fixture for separately requested ticket reads.
    last_operation_status: Option<backend_library::IndexOperationStatus>,
    profile_progress_reply: Option<backend_library::IndexProgressPage>,
    /// Exact captured bytes; responds to the actual selected native range.
    semantic_source_document: Option<std::sync::Arc<backend_library::SemanticSourceDocument>>,
    /// Synthetic immutable membership; portable source bytes remain real42.
    semantic_versions_total: Option<u64>,
    membership_rows: Option<Vec<backend_library::PackageSourceMembershipFileV1>>,
    native_graph_rows: Option<Vec<backend_library::PackageDependencyRecord>>,
    /// Stops the exact admitted scope after a native graph reply is prepared.
    graph_cancel_after_native_reply: Option<RequestCancellationReason>,
    /// Only explicit local-serving metadata authorizes a shell alternative.
    cli_context: Option<backend_present::StartupRecoveryContext>,
    /// Exact sequential observations for bounded-wait controls.
    surface_replies: std::collections::VecDeque<SurfaceReply>,
    /// Signals a real actor progress read before external cancellation.
    surface_admitted: Option<std::sync::mpsc::SyncSender<()>>,
    /// Already-persisted durable identity used by the shared index-driver fixture.
    durable_index_key: Option<backend_library::IndexOperationKey>,
    /// Typed client failure returned by the surface boundary.
    surface_error: Option<ClientError>,
    /// Typed failure at graph-page, prompt-revision, or continuation-encoding admission.
    adapter_error: Option<ClientError>,
    /// Exact boundary reached by an adapter-error route.
    adapter_boundary: Option<&'static str>,
    /// Exercise the opaque owner cursor family behind `backend.surface`.
    surface_index_search_pages: bool,
    /// Override the opaque owner cursor for size-boundary cases.
    surface_index_search_owner_cursor: Option<String>,
    /// Make the owner reject an index-search cursor as stale.
    surface_index_search_stale: bool,
    /// Owner cursors that reached the durable index-search surface.
    surface_index_search_seen: Vec<Option<String>>,
    /// Exact typed surface commands reaching the owner boundary.
    surface_commands: Vec<SurfaceCommand>,
    /// Number of graph requests that reached the product boundary.
    graph_query_calls: usize,
    /// Exact typed input and page bound admitted from a query call.
    graph_query_inputs: Vec<(AdmittedGraphQueryInput, u16)>,
    /// Exact caller lifetimes seen at request admission.
    request_contexts: Vec<RequestContext>,
    /// Exact index operands that reached the owner admission boundary.
    index_paths: Vec<String>,
    /// Number of command probes that reached the product boundary.
    probe_calls: usize,
    /// Real catalog projection used to exercise owner-issued page contracts.
    page_catalog: Option<backend_library::Library>,
    /// Changes only the exact source snapshot before a budget-driven re-read.
    outline_source_after_first_page: Option<backend_library::Library>,
    /// Each cursor keeps its actual selected owner root at issuance.
    catalog_cursors: BTreeMap<String, (PageContinuation, ViewStateRoot)>,
    /// Actual page bounds admitted by the catalog adapter.
    page_limits: Vec<u16>,
    /// Native delivery windows, including every rejected frontier attempt.
    page_windows: Vec<backend_library::QueryWindow>,
    catalog_frontiers: Vec<Option<PageContinuation>>,
    /// Injected transport control; real native selection runs after delivery fits.
    catalog_delivery_ceiling: Option<u16>,
    /// A changed failed-read revision must not be forgotten before a successful retry.
    outline_refusal_basis: Option<backend_library::ViewRevision>,
    /// Exact document coordinates passed back by a text-preview consumer.
    document_coordinates: Vec<String>,
    /// Bind the default document fixture to the requested display coordinate.
    document_matches_request: bool,
    /// Typed useful name stage returned while richer search prepares.
    published_name_readiness: Option<backend_library::PublishedSearchReadiness>,
    /// Explicit declared compiler scope, independent of name-stage readiness.
    published_name_coverage: Option<Vec<Coverage>>,
}

impl Fake {
    fn fixture_catalog(&mut self, path: &str) -> Result<backend_library::Library, ClientError> {
        if self.offline { return Err(unreachable()); }
        if let Some(error) = self.outline_error.take() { return Err(error); }
        if self.empty_outline {
            return Err(ClientError::CommandFailed(backend_library::CommandFailure::NotFound));
        }
        let mut members = vec![module_row(), declaration_row()];
        // A source-availability override supplies a selected native
        // document. Its owner must also be present in the fixture's
        // otherwise complete member projection; omission is a real
        // protocol failure, independent of optional source absence.
        if let Some(document) = &self.document_override
            && !self.omit_document_override_member
            && !members.iter().any(|row| row.id == RowId::Symbol(document.symbol))
        {
            members.push(Row::in_package(
                RowId::Symbol(document.symbol),
                snapshot(Vec::new()).root.basis(),
                package_key(path),
                self.document_coordinates.last().map_or(DECLARATION, String::as_str),
            ));
        }
        members.push(Row::new(RowId::Package(package_key(path)), basis(), path));
        let owner = root(members);
        let cursor = backend_library::Cursor::for_view_root(&owner);
        Ok(backend_library::Library::from_view(owner, cursor).expect("immutable fixture catalog"))
    }

    fn fixture_outline_page(
        &mut self,
        path: &str,
        limit: u16,
        continuation: Option<PageContinuation>,
    ) -> Result<ProjectionPage, ClientError> {
        let library = self.fixture_catalog(path)?;
        let request = backend_library::PageRequest::new(
            library.revision_root(), backend_library::QueryLimit::new(limit).expect("bounded outline"),
        );
        let request = continuation.map_or(request, |cursor| request.with_continuation(cursor));
        let page = library.outline_page(package_key(path), request)
            .map_err(|error| ClientError::Protocol(error.to_string()))?;
        self.next_continuation = page.snapshot.next.map(PageContinuation::from_cursor);
        Ok(page)
    }

    fn catalog_probe(
        &mut self,
        probe: Probe<'_>,
        continuation: Option<PageContinuation>,
        window: Option<backend_library::QueryWindow>,
    ) -> Result<ReplyDto, ClientError> {
        if matches!(probe, Probe::OutlinePage { .. }) && !self.page_limits.is_empty() {
            if let Some(replacement) = self.outline_source_after_first_page.take() {
                self.page_catalog = Some(replacement);
            }
        }
        let Some(catalog) = &self.page_catalog else {
            if let Probe::OutlinePage { path, limit } = probe {
                self.probe_calls += 1;
                let catalog = self.fixture_catalog(path)?;
                let credit = backend_library::QueryLimit::new(limit).expect("outline credit");
                let window = window.unwrap_or(backend_library::QueryWindow::fixed(credit));
                self.page_windows.push(window);
                let request = backend_library::PageRequest::new(catalog.revision_root(), credit)
                    .with_window(window).expect("outline unchanged credit");
                let request = continuation.map_or(request, |cursor| request.with_continuation(cursor));
                let page = catalog.outline_page(package_key(path), request)
                    .map_err(|error| ClientError::Protocol(error.to_string()))?;
                self.next_continuation = page.snapshot.next.map(PageContinuation::from_cursor);
                return Ok(ReplyDto::new(1, CommandReply::ProjectionPage(page)));
            }
            if continuation.is_some() {
                return Err(ClientError::StaleCursor);
            }
            return self.probe(probe);
        };
        if let Probe::OutlinePage { path, limit } = probe {
            self.page_limits.push(limit);
            let request = backend_library::PageRequest::new(
                catalog.revision_root(),
                backend_library::QueryLimit::new(limit).expect("bounded outline"),
            );
            let window = window.unwrap_or(backend_library::QueryWindow::fixed(request.limit()));
            self.page_windows.push(window);
            self.catalog_frontiers.push(continuation);
            if continuation.is_some() && self.catalog_delivery_ceiling.is_some_and(|ceiling| window.delivery().get() > ceiling) {
                return Err(ClientError::ResponseTooLarge {
                    basis: Some(self.outline_refusal_basis.unwrap_or_else(|| catalog.revision_root().into())),
                });
            }
            let request = request.with_window(window).expect("outline unchanged credit");
            let request = continuation.map_or(request, |cursor| request.with_continuation(cursor));
            let page = catalog
                .outline_page(package_key(path), request)
                .map_err(|error| ClientError::Protocol(error.to_string()))?;
            return Ok(ReplyDto::new(1, CommandReply::ProjectionPage(page)));
        }
        let (text, limit, names) = match probe {
            Probe::Search { text, limit } => (text, limit, false),
            Probe::Names { text, limit } => (text, limit, true),
            _ => return self.probe(probe),
        };
        self.page_limits.push(limit);
        let credit = backend_library::QueryLimit::new(limit).expect("admitted limit");
        let window = window.unwrap_or(backend_library::QueryWindow::fixed(credit));
        self.page_windows.push(window);
        self.catalog_frontiers.push(continuation);
        if continuation.is_some()
            && self
                .catalog_delivery_ceiling
                .is_some_and(|ceiling| window.delivery().get() > ceiling)
        {
            return Err(ClientError::ResponseTooLarge {
                basis: Some(catalog.revision_root().into()),
            });
        }
        let reply = if names {
            let mut query = backend_library::NameQuery::new(text, catalog.revision_root(), credit)
                .with_window(window)
                .expect("name delivery preserves credit");
            if let Some(cursor) = continuation {
                query = query.with_cursor(cursor.cursor());
            }
            catalog.names(&query).map(CommandReply::Names)
        } else {
            let mut query = backend_library::Query::new(text, catalog.revision_root(), credit)
                .with_window(window)
                .expect("search delivery preserves credit");
            if let Some(cursor) = continuation {
                query = query.with_cursor(cursor.cursor());
            }
            selected_search_page(catalog, &query).map(CommandReply::Search)
        }
        .map_err(|error| ClientError::Protocol(error.to_string()))?;
        Ok(ReplyDto::new(1, reply))
    }

    fn catalog_graph_page(
        &mut self,
        coordinate: String,
        limit: u16,
        continuation: Option<PageContinuation>,
        window: Option<backend_library::QueryWindow>,
    ) -> Result<ReplyDto, ClientError> {
        if let Some(failure) = &self.graph_failure {
            return Ok(ReplyDto::new(1, CommandReply::Failed(failure.clone())));
        }

        self.adapter_boundary = Some("graph_page");
        if let Some(error) = self.adapter_error.take() {
            return Err(error);
        }
        if let Some(catalog) = &self.page_catalog {
            self.page_limits.push(limit);
            let mut request = backend_library::PageRequest::new(
                catalog.revision_root(),
                backend_library::QueryLimit::new(limit).expect("admitted limit"),
            );
            let window = window.unwrap_or(backend_library::QueryWindow::fixed(request.limit()));
            self.page_windows.push(window);
            self.catalog_frontiers.push(continuation);
            if continuation.is_some()
                && self
                    .catalog_delivery_ceiling
                    .is_some_and(|ceiling| window.delivery().get() > ceiling)
            {
                return Err(ClientError::ResponseTooLarge {
                    basis: Some(catalog.revision_root().into()),
                });
            }
            request = request
                .with_window(window)
                .expect("graph delivery preserves credit");
            if let Some(cursor) = continuation {
                request = request.with_continuation(cursor);
            }
            let page = catalog
                .graph_page(symbol_key(&coordinate), request)
                .map_err(|error| ClientError::Protocol(error.to_string()))?;
            return Ok(ReplyDto::new(1, CommandReply::ProjectionPage(page)));
        }
        if coordinate == MISSING {
            return Err(ClientError::CommandFailed(
                backend_library::CommandFailure::NotFound,
            ));
        }
        Ok(ReplyDto::new(
            1,
            CommandReply::ProjectionPage(ProjectionPage {
                outline_order: None,
                snapshot: snapshot(vec![declaration_row()]),
                terminal: PageTerminal::Complete,
            }),
        ))
    }
}

fn basis() -> Basis {
    Basis::new(view_state_root(&[]), object_version(b"source"))
}

fn index_job_ticket() -> IndexJobTicket {
    IndexJobTicket::new(
        std::num::NonZeroU64::new(17).expect("nonzero ticket"),
        [7; 16],
        PackageReference::parse("pkg:cargo/serde@1.0.228").expect("pinned package"),
    )
}

fn ticket_value(ticket: &IndexJobTicket) -> Value {
    serde_json::to_value(ticket).expect("owner ticket serializes")
}

fn root(rows: Vec<Row>) -> ViewRoot {
    let basis = basis();
    ViewRoot::new_incomplete(
        view_key(b"mcp-fixture"),
        basis,
        Frontier::new(
            basis.branch,
            basis.log,
            basis.schema,
            view_state_root(&[]),
            0,
        ),
        rows,
        vec![Coverage::Unavailable {
            lane: Lane::Semantic,
            reason: Reason::Unconfigured,
        }],
    )
    .expect("fixture view root")
}

fn snapshot(rows: Vec<Row>) -> ViewSnapshot {
    ViewSnapshot {
        root: root(rows),
        freshness: Freshness::Current,
        next: None,
        graph_relations: None,
        rich_graph: None,
    }
}

fn package_row() -> Row {
    Row::new(RowId::Package(package_key(PROJECT)), basis(), PROJECT)
}

fn module_row() -> Row {
    Row::in_package(
        RowId::Symbol(symbol_key(MODULE)),
        basis(),
        package_key(PROJECT),
        MODULE,
    )
    .with_kind(DeclarationKind::Module)
}

fn declaration_row() -> Row {
    Row::in_package(
        RowId::Symbol(symbol_key(DECLARATION)),
        basis(),
        package_key(PROJECT),
        DECLARATION,
    )
    .with_parent(symbol_key(MODULE))
    .with_kind(DeclarationKind::Function)
    .with_signature("pub fn ferris() -> Beacon")
    .with_document([Fragment::Text("Lights the beacon.".to_owned())])
    .with_source(SourceLocation::new("src/lib.rs", 2).expect("one-based location"))
    .with_excerpt(
        SourceExcerpt::captured(
            "pub fn ferris() -> Beacon {\n    Beacon\n}",
            SourceExcerptExtent::Complete,
        )
        .expect("bounded excerpt"),
    )
}

fn document() -> Document {
    let mut document = Document::new(
        symbol_key(MODULE),
        view_state_root(&[]),
        [Fragment::Text("Lights the beacon.".to_owned())],
    )
    .with_location(SourceAvailability::Captured(
        SourceLocation::new("src/lib.rs", 2).expect("one-based location"),
    ))
    .with_excerpt(
        SourceExcerpt::captured(
            "pub fn ferris() -> Beacon {\n    Beacon\n}",
            SourceExcerptExtent::Complete,
        )
        .expect("bounded excerpt"),
    );
    document.symbol = symbol_key(MODULE);
    document.signature = Some("pub mod lib".to_owned());
    document
}

fn unreachable() -> ClientError {
    ClientError::Io("no such file or directory".to_owned())
}

impl Engine for Fake {
    fn members_page(
        &mut self,
        document: &backend_library::Document,
        limit: u16,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        if !self.page_limits.is_empty()
            && let Some(replacement) = self.outline_source_after_first_page.take()
        {
            self.page_catalog = Some(replacement);
        }
        let fixture = if self.page_catalog.is_none() {
            self.probe_calls += 1;
            Some(self.fixture_catalog(PROJECT)?)
        } else {
            self.page_limits.push(limit);
            None
        };
        let catalog = self.page_catalog.as_ref().or(fixture.as_ref())
            .expect("explicit or default immutable fixture catalog");
        let request = backend_library::PageRequest::new(
            document.basis,
            backend_library::QueryLimit::new(limit).expect("credit"),
        );
        let request = continuation.map_or(request, |cursor| request.with_continuation(cursor));
        let mut owner = backend_library::DocumentQuery::selected(document.symbol, document.basis);
        if let Some(source) = document.source_basis() {
            owner = owner.with_source_basis(source);
        }
        let page = catalog.members_page(owner, request)
            .map_err(|error| ClientError::Protocol(error.to_string()))?;
        self.next_continuation = page.snapshot.next.map(PageContinuation::from_cursor);
        Ok(ReplyDto::new(1, CommandReply::ProjectionPage(page)))
    }
    fn index(
        &mut self,
        package: PackageReference,
        execution_intent: CompileExecutionIntent,
    ) -> Result<SurfaceReply, ClientError> {
        let command = self.durable_index_key.map_or_else(
            || SurfaceCommand::IndexStart {
                package: package.clone(),
                execution_intent,
            },
            |operation_key| SurfaceCommand::IndexOperationStart {
                operation_key,
                package: package.clone(),
                execution_intent,
            },
        );
        self.surface(command)
    }

    fn revision(&mut self) -> Result<ViewStateRoot, ClientError> {
        self.adapter_boundary = Some("revision");
        if let Some(error) = self.adapter_error.take() {
            return Err(error);
        }
        Ok(self.page_catalog.as_ref().map_or_else(
            || view_state_root(&[]),
            backend_library::Library::revision_root,
        ))
    }

    fn health(&mut self) -> Result<HealthReport, ClientError> {
        if let Some(calls) = &self.health_calls {
            calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        let library = backend_library::Library::new();
        Ok(HealthReport::from_root(library.view(), library.cursor()))
    }

    fn probe(&mut self, probe: Probe<'_>) -> Result<ReplyDto, ClientError> {
        self.probe_calls += 1;
        if let Probe::Index(path) | Probe::IndexWithExecutionIntent { path, .. } = probe {
            self.index_paths.push(path.to_owned());
        }
        if self.offline {
            return Err(unreachable());
        }
        let reply = match probe {
            Probe::Declaration { selector, read } => {
                let Some(catalog) = &self.page_catalog else {
                    return Err(ClientError::Protocol("selected read needs the immutable-row fixture".to_owned()));
                };
                let symbol = selector.address().resolve(catalog.view()).ok_or(ClientError::CommandFailed(backend_library::CommandFailure::NotFound))?;
                let row = catalog.view().row(RowId::Symbol(symbol)).ok_or(ClientError::CommandFailed(backend_library::CommandFailure::NotFound))?;
                if selector.coordinate().is_some_and(|coordinate| coordinate != row.label) {
                    return Err(ClientError::Protocol("selected coordinate assertion mismatch".to_owned()));
                }
                match read {
                    backend_library::DeclarationRead::Document | backend_library::DeclarationRead::Source => {
                        let document = catalog.document(backend_library::DocumentQuery::selected(symbol, catalog.revision_root())).map_err(|error| ClientError::Protocol(error.to_string()))?;
                        let selection = backend_library::DocumentSelection::from_row(&row, document.source_basis().expect("source")).expect("selected label");
                        return Ok(ReplyDto::new(1, CommandReply::Document(document.with_selection(selection).expect("exact selected row"))));
                    }
                    backend_library::DeclarationRead::Graph | backend_library::DeclarationRead::Related => {
                        return Ok(ReplyDto::new(1, CommandReply::Graph(snapshot(vec![declaration_row()]))));
                    }
                }
            }
            Probe::Packages => CommandReply::Packages(snapshot(vec![
                package_row(),
                module_row(),
                declaration_row(),
            ])),
            Probe::Index(_) => {
                CommandReply::Added(Intent::request_package(package_key(PROJECT)).id())
            }
            Probe::IndexWithExecutionIntent { .. } => {
                CommandReply::Added(Intent::request_package(package_key(PROJECT)).id())
            }
            Probe::Remove(_) => {
                CommandReply::Removed(Intent::remove_package(package_key(PROJECT)).id())
            }
            Probe::Document(at) => {
                self.document_coordinates.push(at.to_owned());
                if at == MISSING {
                    return Err(ClientError::CommandFailed(
                        backend_library::CommandFailure::NotFound,
                    ));
                }
                let mut value = self.document_override.clone().unwrap_or_else(document);
                if self.document_matches_request && self.document_override.is_none() {
                    value.symbol = symbol_key(at);
                }
                CommandReply::Document(value)
            }
            Probe::Source(at) => {
                if at == MISSING {
                    return Err(ClientError::CommandFailed(
                        backend_library::CommandFailure::NotFound,
                    ));
                }
                CommandReply::Document(self.document_override.clone().unwrap_or_else(document))
            }
            Probe::Related(at) | Probe::Graph(at) => {
                if at == MISSING {
                    return Err(ClientError::CommandFailed(
                        backend_library::CommandFailure::NotFound,
                    ));
                }
                CommandReply::Graph(snapshot(vec![declaration_row()]))
            }
            Probe::Search { .. } => match self.published_name_readiness {
                Some(readiness) => {
                    let mut snapshot = snapshot(vec![declaration_row()]);
                    if let Some(coverage) = &self.published_name_coverage {
                        snapshot.root = ViewRoot::new_incomplete(
                            snapshot.root.recipe(), snapshot.root.basis(), snapshot.root.frontier(),
                            snapshot.root.rows().to_vec(), coverage.clone(),
                        ).expect("explicit name-stage declared scope");
                    }
                    CommandReply::PublishedNameSearch(backend_library::PublishedNameSearchPage {
                        snapshot, readiness,
                    })
                },
                None => CommandReply::Search(snapshot(vec![declaration_row()])),
            },
            Probe::Names { .. } => CommandReply::Names(snapshot(vec![declaration_row()])),
            Probe::Outline(path) => {
                // Legacy resource trees and their native name pages must attest
                // the same immutable source, including its complete basis.
                let catalog = self.fixture_catalog(path)?;
                CommandReply::Outline(catalog.outline(backend_library::OutlineQuery::new(
                    package_key(path), catalog.revision_root(),
                )).map_err(|error| ClientError::Protocol(error.to_string()))?)
            }
            Probe::OutlinePage { path, limit } => {
                CommandReply::ProjectionPage(self.fixture_outline_page(path, limit, None)?)
            }
        };
        Ok(ReplyDto::new(1, reply))
    }

    fn probe_page(
        &mut self,
        probe: Probe<'_>,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        self.catalog_probe(probe, continuation, None)
    }

    fn probe_page_window(
        &mut self,
        probe: Probe<'_>,
        continuation: Option<PageContinuation>,
        window: backend_library::QueryWindow,
    ) -> Result<ReplyDto, ClientError> {
        self.catalog_probe(probe, continuation, Some(window))
    }

    fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
        if let Some(counter) = &self.surface_dispatches {
            counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        self.surface_commands.push(command.clone());
        if let (SurfaceCommand::PackageSourceMembership { request }, Some(rows)) =
            (&command, &self.membership_rows)
        {
            return Ok(SurfaceReply::PackageSourceMembershipPage(
                source_membership_pages::page_for(request, rows),
            ));
        }
        if let (SurfaceCommand::SemanticVersions { limit, cursor, .. }, Some(total)) =
            (&command, self.semantic_versions_total)
        {
            let offset = cursor
                .as_ref()
                .map(|cursor| cursor.claims().expect("synthetic owner cursor").offset)
                .unwrap_or(0);
            return Ok(SurfaceReply::SemanticVersionsPage(
                semantic_source_pages::page(*limit, offset, total, 8),
            ));
        }
        if let (SurfaceCommand::SemanticSource { request }, Some(document)) =
            (&command, &self.semantic_source_document)
        {
            let chunk = document
                .chunk(&request.range)
                .map_err(|error| ClientError::Protocol(error.to_string()))?;
            return Ok(SurfaceReply::SemanticSource(chunk));
        }
        if let Some(admitted) = self.surface_admitted.take() {
            admitted.send(()).expect("actual progress read admitted");
        }
        if let SurfaceCommand::IndexProgress {
            ticket,
            after_sequence,
        } = &command
        {
            if !matches!(
                self.surface_replies.front(),
                Some(SurfaceReply::IndexProgress(_))
            ) && !matches!(&self.surface_reply, Some(SurfaceReply::IndexProgress(_)))
            {
                if let Some(status) = &self.last_operation_status {
                    if matches!(&status.state, backend_library::IndexOperationState::Active { ticket: active, .. } if active == ticket)
                    {
                        let stage = match &status.state {
                            backend_library::IndexOperationState::Active { stage, .. } => *stage,
                            _ => unreachable!(),
                        };
                        let page = self.profile_progress_reply.clone().unwrap_or_else(|| {
                            backend_library::IndexProgressPage {
                                ticket: ticket.clone(),
                                stage,
                                events: Box::new([]),
                                next_sequence: *after_sequence,
                                truncated: false,
                                has_more: false,
                            }
                        });
                        return Ok(SurfaceReply::IndexProgress(
                            backend_library::IndexJobObservation::Pending(page),
                        ));
                    }
                }
            }
        }
        if let Some(error) = self.surface_error.take() {
            return Err(error);
        }
        if let Some(reply) = self
            .surface_replies
            .pop_front()
            .or_else(|| self.surface_reply.take())
        {
            if let SurfaceReply::IndexOperationStatus(
                backend_library::IndexOperationObservation::Known(status),
            ) = &reply
            {
                self.last_operation_status = Some(status.clone());
            }
            return Ok(reply);
        }
        if let (SurfaceCommand::PackageGraphPage { request }, Some(rows)) =
            (&command, &self.native_graph_rows)
        {
            let reply =
                SurfaceReply::PackageGraphPage(package_graph_budget::page_for(request, rows));
            if let Some(reason) = self.graph_cancel_after_native_reply.take() {
                self.request_contexts
                    .last()
                    .expect("original admitted graph lifetime")
                    .cancel(reason);
            }
            return Ok(reply);
        }
        match command {
            SurfaceCommand::IndexStart { package, .. } => {
                self.index_paths.push(package.as_str().to_owned());
                if self.offline {
                    return Err(unreachable());
                }
                Ok(SurfaceReply::IndexStarted(IndexStartResult::Started {
                    ticket: IndexJobTicket::new(std::num::NonZeroU64::MIN, [7; 16], package),
                    stage: backend_library::IndexJobStage::Scanning,
                }))
            }
            SurfaceCommand::IndexSearch { cursor, .. } if self.surface_index_search_pages => {
                self.surface_index_search_seen
                    .push(cursor.as_ref().map(|cursor| cursor.as_str().to_owned()));
                if cursor.is_some() && self.surface_index_search_stale {
                    return Err(ClientError::StaleCursor);
                }
                let expected_cursor = self
                    .surface_index_search_owner_cursor
                    .as_deref()
                    .unwrap_or(FIXTURE_INDEX_CURSOR);
                if let Some(cursor) = &cursor
                    && cursor.as_str() != expected_cursor
                {
                    return Err(ClientError::Protocol(
                        "fixture received a non-owner index-search cursor".to_owned(),
                    ));
                }
                let next_cursor = cursor.is_none().then(|| {
                    IndexSearchCursor::new(expected_cursor.to_owned()).expect("owner cursor")
                });
                Ok(SurfaceReply::IndexSearchPage(IndexSearchPage {
                    snapshot: [42; 32],
                    evaluated_at_millis: 17,
                    hits: Box::new([]),
                    next_cursor,
                    result_count: IndexSearchResultCount::AtLeast(1),
                }))
            }
            SurfaceCommand::Subscriptions => Ok(SurfaceReply::Subscriptions(Box::new([]))),
            SurfaceCommand::TreeOpen {
                subject,
                parent,
                title,
                opener,
            } => Ok(SurfaceReply::TreeOpened(backend_library::TreeNodeRecord {
                id: backend_library::TreeNodeId::new(std::num::NonZeroU64::MIN),
                parent,
                subject,
                title: title.unwrap_or_else(|| ProductText::new("fixture node").expect("title")),
                opener,
                active: true,
            })),
            SurfaceCommand::References { target } => Ok(SurfaceReply::References {
                target,
                references: Box::new([backend_library::ReferenceRecord {
                    site: backend_library::ProductText::new("pkg::semantic::caller")
                        .expect("site text"),
                    target: backend_library::SemanticLinkTarget::Local {
                        declaration: backend_library::SemanticDeclarationIdentity {
                            family: [1; 16],
                            variant: [2; 16],
                        },
                    },
                    relation: backend_library::SemanticLinkKind::Calls,
                    evidence: backend_library::SemanticLinkEvidence {
                        confidence: backend_library::SemanticConfidence::Compiler,
                        source: Some(backend_library::SemanticSourceSpan {
                            file: backend_library::ProductText::new("src/main.rs")
                                .expect("path text"),
                            start: 40,
                            end: 46,
                        }),
                    },
                }]),
            }),
            SurfaceCommand::Diff { .. } => Err(ClientError::CommandFailed(
                backend_library::CommandFailure::InvalidQuery(
                    "the package is not indexed at this revision".to_owned(),
                ),
            )),
            other => Err(ClientError::Protocol(format!(
                "fixture has no reply for {:?}",
                other.id()
            ))),
        }
    }
}

impl Product for Fake {
    fn set_request_context(&mut self, context: Option<RequestContext>) -> Result<(), ClientError> {
        if let Some(context) = context {
            context.check()?;
            self.request_contexts.push(context);
        }
        Ok(())
    }
    fn cli_context(&self) -> Option<backend_present::StartupRecoveryContext> {
        self.cli_context.clone()
    }

    fn encode_continuation(
        &mut self,
        continuation: backend_library::PageContinuation,
    ) -> Result<String, ClientError> {
        self.adapter_boundary = Some("encode_continuation");
        if let Some(error) = self.adapter_error.take() {
            return Err(error);
        }
        if let Some(catalog) = &self.page_catalog {
            let token = fixture_page_cursor(continuation);
            self.catalog_cursors
                .insert(token.clone(), (continuation, catalog.revision_root()));
            return Ok(token);
        }
        if self.next_continuation == Some(continuation) {
            Ok(fixture_page_cursor(continuation))
        } else {
            Err(ClientError::Protocol("unknown fixture cursor".to_owned()))
        }
    }

    fn graph_page(
        &mut self,
        coordinate: String,
        limit: u16,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        self.catalog_graph_page(coordinate, limit, continuation, None)
    }

    fn graph_page_window(
        &mut self,
        coordinate: String,
        window: backend_library::QueryWindow,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        self.catalog_graph_page(
            coordinate,
            window.issued().get(),
            continuation,
            Some(window),
        )
    }

    fn outline_page_token(
        &mut self,
        _coordinate: String,
        _window: backend_library::QueryWindow,
        token: &str,
    ) -> Result<ReplyDto, ClientError> {
        self.outline_owner_tokens.push(token.to_owned());
        Err(ClientError::Protocol(
            "PC5 owner route reached test product".to_owned(),
        ))
    }

    fn decode_continuation(
        &mut self,
        token: &str,
    ) -> Result<backend_library::PageContinuation, ClientError> {
        if let Some(error) = &self.continuation_error {
            return Err(error.clone());
        }
        if let Some(catalog) = &self.page_catalog {
            let (cursor, root) = self
                .catalog_cursors
                .get(token)
                .ok_or_else(|| ClientError::Protocol("unknown catalog cursor".to_owned()))?;
            if *root != catalog.revision_root() {
                return Err(ClientError::StaleCursor);
            }
            return Ok(*cursor);
        }
        if self
            .next_continuation
            .is_some_and(|cursor| token == fixture_page_cursor(cursor))
        {
            if self.stale_cursor {
                return Err(ClientError::StaleCursor);
            }
            self.next_continuation
                .ok_or_else(|| ClientError::Protocol("fixture has no cursor".to_owned()))
        } else {
            Err(ClientError::Protocol("unknown fixture cursor".to_owned()))
        }
    }

    fn graph_query(
        &mut self,
        input: AdmittedGraphQueryInput,
        limit: u16,
        continuation: Option<backend_library::PageContinuation>,
    ) -> Result<GraphQueryPage, ClientError> {
        self.graph_query_inputs.push((input, limit));
        if let Some(error) = &self.graph_query_error {
            return Err(error.clone());
        }

        self.graph_query_calls += 1;
        if let Some(rows) = &self.graph_paged_rows {
            let offset = continuation.map_or(0, |cursor| cursor.cursor().sequence() as usize);
            let end = rows.len().min(offset + usize::from(limit));
            let terminal = if end < rows.len() {
                let next = PageContinuation::from_cursor(backend_library::Cursor::at(
                    view_state_root(&[]),
                    end as u64,
                ));
                self.next_continuation = Some(next);
                PageTerminal::More(next)
            } else {
                PageTerminal::Complete
            };
            return Ok(GraphQueryPage {
                revision: if self.graph_query_source_changes && self.graph_query_calls > 1 {
                    view_state_root(&[("changed".to_owned(), "native source".to_owned())]).into()
                } else {
                    view_state_root(&[]).into()
                },
                source: basis().object,
                rows: rows[offset..end].to_vec().into_boxed_slice(),
                terminal,
            });
        }
        if let Some(rows) = &self.graph_rows {
            return Ok(GraphQueryPage {
                revision: view_state_root(&[]).into(),
                source: basis().object,
                rows: rows.clone(),
                terminal: PageTerminal::Complete,
            });
        }
        if self.graph_continue {
            if continuation.is_none() {
                let continuation = PageContinuation::from_cursor(backend_library::Cursor::at(
                    view_state_root(&[]),
                    1,
                ));
                self.next_continuation = Some(continuation);
                return Ok(GraphQueryPage {
                    revision: view_state_root(&[]).into(),
                    source: basis().object,
                    rows: Box::new([]),
                    terminal: PageTerminal::More(continuation),
                });
            }
            assert_eq!(continuation, self.next_continuation);
        }
        Ok(GraphQueryPage {
            revision: view_state_root(&[]).into(),
            source: basis().object,
            rows: Box::new([]),
            terminal: PageTerminal::Complete,
        })
    }
}

// ---------------------------------------------------------------------------
// driving the server
// ---------------------------------------------------------------------------
fn ready(product: Fake) -> Server<Fake> {
    let mut server = Server::with_authority(product, PROJECT.to_owned(), [9; 32]);
    server
        .handle(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#)
        .expect("initialize response");
    assert!(
        server
            .handle(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_none()
    );
    server
}

fn request(server: &mut Server<Fake>, method: &str, params: &Value) -> Value {
    let message = json!({ "jsonrpc": "2.0", "id": 9, "method": method, "params": params });
    server
        .handle(
            serde_json::to_vec(&message)
                .expect("encode request")
                .as_slice(),
        )
        .map(|reply| encoded_stdio_reply(reply))
        .expect("a request receives a response")
}

fn encoded_stdio_reply(reply: Value) -> Value {
    let mut line = Vec::new();
    write_message(&mut line, &reply).expect("production stdio encoder");
    serde_json::from_slice(&line).expect("encoded stdio response")
}

fn call(server: &mut Server<Fake>, tool: &str, arguments: &Value) -> Value {
    request(
        server,
        "tools/call",
        &json!({ "name": tool, "arguments": arguments }),
    )["result"]
        .clone()
}

fn native_transport_source_page(
    source: &backend_library::SemanticVersionRecord,
) -> backend_library::SemanticVersionsPage {
    let source_root = source
        .selected_source_frontier
        .as_ref()
        .map_or([8; 32], |frontier| frontier.source_relation_root);
    backend_library::SemanticVersionsPage {
        package: source.package.clone(),
        source_relation_root: source_root,
        semantic_relation_root: [34; 32],
        total: 1,
        offset: 0,
        entries: Box::new([backend_library::SemanticVersionDescriptor::Available {
            reference: backend_library::SemanticSourceReference::capture(
                source,
                source_root,
                [34; 32],
                &[7; 32],
            )
            .expect("transport selector"),
        }]),
        next_cursor: None,
        more: false,
        complete: true,
    }
}

#[test]
fn semantic_shapes_actual_jsonrpc_preserves_full_view_in_summary_and_full() {
    let operands: Value = serde_json::from_str(include_str!(
        "../../../../crates/library/fixtures/semantic-shape-read.json"
    ))
    .expect("shared operands fixture");
    for detail in ["summary", "full"] {
        let selected: backend_library::SemanticShapeReadRequest =
            serde_json::from_value(operands.clone()).expect("request");
        let backend_library::SemanticShapeSource::Inline(source) = selected.source() else {
            panic!("inline fixture")
        };
        let mut server = ready(Fake {
            surface_reply: Some(SurfaceReply::SemanticVersionsPage(
                native_transport_source_page(source),
            )),
            ..Fake::default()
        });
        let resolved = call(
            &mut server,
            "backend.resolve",
            &json!({"query":"ferris","detail":detail}),
        );
        let selector =
            resolved["structuredContent"]["records"][0]["identity"]["semantic_data"].clone();
        assert_eq!(selector["kind"], "selected-symbol-id");
        assert_eq!(selector["value"], json!(symbol_key(DECLARATION).as_bytes()));
        let versions = call(
            &mut server,
            "backend.semantic_versions",
            &json!({"package":"/abs/shape-fixture","detail":detail}),
        );
        let reference =
            &versions["structuredContent"]["semantic_data"]["value"]["entries"][0]["reference"];
        assert_eq!(
            reference["metadata"]["generation"],
            operands["source"]["generation"]
        );
        assert_eq!(
            reference["metadata"]["coordinate"],
            operands["source"]["coordinate"]
        );
        // The inline/current-shape epoch path remains separately supported.
        // Full reference retrieval and exact shape source selection are covered
        // by source range/cold-owner controls rather than fabricated here.
        let mut shape_operands = operands.clone();
        shape_operands["symbols"] = json!([selector["value"]]);
        let mut export_view: Value = serde_json::from_str(include_str!(
            "../../../../crates/library/fixtures/semantic-shape-egress-view.json"
        ))
        .expect("untrusted unavailable display fixture");
        export_view["batch"]["entries"][0]["symbol"]["id"] = json!(backend_library::encode_id(
            symbol_key(DECLARATION).as_bytes()
        ));
        let export: backend_library::SemanticShapeExport = serde_json::from_value(export_view)
            .expect("closed display selector from actual resolve output");
        server.product.surface_reply = Some(SurfaceReply::SemanticShapes(export.clone()));
        let result = call(
            &mut server,
            "backend.semantic_shapes",
            &json!({"request":shape_operands,"detail":detail}),
        );
        assert_ne!(result["isError"], true, "{result}");
        assert_eq!(
            result["structuredContent"]["semantic_data"]["value"],
            serde_json::to_value(&export).expect("view")
        );
        assert_eq!(server.product.surface_commands.len(), 2);
        assert!(
            matches!(&server.product.surface_commands[1], SurfaceCommand::SemanticShapes { request }
            if request.symbols() == &[*symbol_key(DECLARATION).as_bytes()])
        );
    }
    let mut server = ready(Fake::default());
    let mut wrong = operands.clone();
    wrong["symbols"] = json!([{"kind":"canonical","id":"06".repeat(32)}]);
    let result = request(
        &mut server,
        "tools/call",
        &json!({"name":"backend.semantic_shapes","arguments":{"request":wrong}}),
    );
    assert_eq!(result["result"]["isError"], true, "{result}");
    assert_eq!(result["result"]["structuredContent"]["slug"], "usage");
    assert_eq!(result["result"]["structuredContent"]["cause"], "malformed");
    assert!(server.product.surface_commands.is_empty());
}

#[test]
fn archived_semantic_versions_captured_semver_projection_keeps_dense_transport_refusal() {
    let packet: Value = serde_json::from_str(include_str!(
        "../../../../crates/present/fixtures/semantic-versions-semver-public.json"
    ))
    .expect("complete captured public source packet");
    let reply: SurfaceReply = serde_json::from_value(packet["surface"].clone())
        .expect("complete source DTO; no wire certificate is fabricated");
    let answer = Answer::Product(Box::new(backend_present::product_view(&reply)));
    let SurfaceReply::SemanticVersions(records) = &reply else {
        panic!("captured versions source");
    };
    let package = packet["surface"]["data"][0]["package"]["value"]
        .as_str()
        .expect("exact captured local package");
    assert_eq!(
        packet["surface"]["data"][0]["history_status"]["proof"]["images"]
            .as_array()
            .expect("all actual captured image proofs")
            .len(),
        42
    );
    // Preserve the complete persisted/archived DTO and CLI parity. Live
    // discovery now returns a native descriptor page; archived source blobs
    // remain an honest whole-envelope refusal control.
    for requested_detail in [
        None,
        Some(Detail::Summary),
        Some(Detail::Standard),
        Some(Detail::Full),
    ] {
        let detail = requested_detail.unwrap_or(Detail::Summary);
        let mut server = ready(Fake::default());
        let mut arguments = json!({"package":package});
        if requested_detail.is_some() {
            arguments["detail"] = json!(detail.name());
        }
        let context = continuation_context(
            PROJECT,
            "backend.semantic_versions",
            arguments.as_object().expect("arguments"),
            detail,
        );
        let candidate = codec::success(
            json!(9),
            server
                .rendered(
                    Answer::Product(Box::new(backend_present::product_view(&reply))),
                    detail,
                    &context,
                    CursorTarget::CliOption,
                    McpInvocation::new(
                        McpExposure::Direct,
                        "backend.semantic_versions",
                        arguments.as_object().expect("arguments"),
                    ),
                )
                .expect("archived typed DTO display before whole-envelope admission"),
        );
        let result = &candidate["result"];
        assert_eq!(result["isError"], false, "{candidate}");
        assert_eq!(
            result["structuredContent"]["semantic_data"]["value"],
            packet["surface"]["data"]
        );
        assert!(
            result["structuredContent"]["records"][0]["history_status"]["proof"]
                .get("images")
                .is_none()
        );
        assert_eq!(
            result["structuredContent"]["records"]
                .as_array()
                .expect("whole version set")
                .len(),
            1
        );
        assert_eq!(
            result["structuredContent"]["records"][0]["title"],
            packet["surface"]["data"][0]["coordinate"]
        );
        assert_eq!(
            result["structuredContent"]["records"][0]["operand"],
            backend_library::encode_id(&records[0].generation.to_bytes())
        );
        let dto: backend_present::ProductDto =
            serde_json::from_value(result["structuredContent"].clone())
                .expect("exact captured public version DTO before transport admission");
        assert_eq!(
            dto.semantic_data,
            Some(backend_present::ProductSemanticData::Versions(
                records.clone()
            ))
        );
        // The CLI adapter regression compares its actual JSON bytes to this
        // same production encoder and its Markdown to this shared renderer.
        let cli_projection = backend_present::encode_answer(
            &answer,
            detail,
            None,
            backend_present::DEFAULT_RESPONSE_BUDGET_BYTES,
        )
        .expect("shared CLI/MCP product projection");
        let cli_projection: Value =
            serde_json::from_slice(&cli_projection.bytes).expect("CLI JSON");
        assert_eq!(result["structuredContent"], cli_projection);
        assert_eq!(
            text_of(result),
            backend_present::bounded_text(&backend_present::markdown::answer(&answer))
        );
        assert_context_bounded(&candidate);
        let mut without_preview = candidate.clone();
        without_preview["result"]["content"] = json!([]);
        assert!(
            wire_tokens(&without_preview).expect("byte-admissible dense operand tokens")
                > MCP_CL100K_TOKEN_LIMIT,
            "shrinking preview cannot make the exact source operand fit"
        );
        let response = encoded_stdio_reply(candidate);
        assert_context_bounded(&response);
        assert!(response.get("result").is_none(), "{response}");
        assert_eq!(response["error"]["code"], -32000);
        let refusal = &response["error"]["data"]["structuredContent"];
        assert_eq!(refusal["slug"], "transport");
        assert_eq!(refusal["cause"], "oversized");
        assert!(refusal.get("semantic_data").is_none());
        assert!(
            refusal.get("nextCursor").is_none(),
            "no fabricated semantic-source continuation"
        );
        assert!(wire_tokens(&response).expect("actual bounded refusal") <= MCP_CL100K_TOKEN_LIMIT);
        assert!(
            server.product.surface_commands.is_empty(),
            "archived display is not a current owner call"
        );
    }
}

#[test]
fn semantic_versions_default_discovers_real_42_image_capture_without_embedding_source_blob() {
    let packet: Value = serde_json::from_str(include_str!(
        "../../../../crates/present/fixtures/semantic-versions-semver-public.json"
    ))
    .expect("unchanged native 42-image capture");
    let source: backend_library::SemanticVersionRecord =
        serde_json::from_value(packet["surface"]["data"][0].clone())
            .expect("exact captured record");
    let source_root = source
        .selected_source_frontier
        .as_ref()
        .expect("real captured frontier")
        .source_relation_root;
    // Fixture seal is a transport selector only. Native owner/cold authority is
    // exercised by the real local-service journey, never inferred from Fake.
    let reference =
        backend_library::SemanticSourceReference::capture(&source, source_root, [34; 32], &[7; 32])
            .expect("frozen compact source selector");
    let page = backend_library::SemanticVersionsPage {
        package: source.package.clone(),
        source_relation_root: source_root,
        semantic_relation_root: [34; 32],
        total: 1,
        offset: 0,
        entries: Box::new([backend_library::SemanticVersionDescriptor::Available {
            reference: reference.clone(),
        }]),
        next_cursor: None,
        more: false,
        complete: true,
    };
    let reply = SurfaceReply::SemanticVersionsPage(page.clone());
    let answer = Answer::Product(Box::new(backend_present::product_view(&reply)));
    for detail in [Detail::Summary, Detail::Standard, Detail::Full] {
        let mut server = ready(Fake {
            surface_reply: Some(reply.clone()),
            ..Fake::default()
        });
        let response = request(
            &mut server,
            "tools/call",
            &json!({"name":"backend.semantic_versions","arguments":{"package":source.package.as_str(),"detail":detail.name()}}),
        );
        assert_context_bounded(&response);
        assert!(
            wire_tokens(&response).expect("actual whole envelope tokens") <= MCP_CL100K_TOKEN_LIMIT
        );
        let result = &response["result"];
        assert_eq!(result["isError"], false, "{response}");
        let dto: backend_present::ProductDto =
            serde_json::from_value(result["structuredContent"].clone())
                .expect("real typed public decode");
        if detail == Detail::Summary {
            assert!(dto.semantic_data.is_none());
            let summary = dto.semantic_versions_summary.as_ref().expect("display discovery");
            assert_eq!(summary.entries[0].coordinate, source.coordinate.as_str());
            assert_eq!(summary.entries[0].history_images, Some(42));
            assert_eq!(summary.full_detail.scope, "fresh-discovery");
        } else {
            assert_eq!(dto.semantic_data,
                Some(backend_present::ProductSemanticData::VersionsPage(page.clone())));
        }
        assert!(text_of(result).contains(source.coordinate.version()));
        assert!(text_of(result).contains("42 image(s)"));
        if detail != Detail::Summary {
            let exposed =
                &result["structuredContent"]["semantic_data"]["value"]["entries"][0]["reference"];
            assert_eq!(exposed["metadata"]["generation"], json!(source.generation));
            assert_eq!(exposed["metadata"]["coordinate"], json!(source.coordinate));
            assert_eq!(exposed["source"], json!(reference.source));
            assert!(exposed["history"]["proof"].get("images").is_none());
            assert_eq!(exposed["history"]["proof"]["image_count"], 42);
        }
        let cli =
            backend_present::encode_answer(&answer, detail, None, DEFAULT_RESPONSE_BUDGET_BYTES)
                .expect("same shared CLI projection");
        assert_eq!(
            result["structuredContent"],
            serde_json::from_slice::<Value>(&cli.bytes).expect("CLI typed JSON")
        );
        assert!(matches!(
            &server.product.surface_commands[..],
            [SurfaceCommand::SemanticVersions {
                limit: backend_library::DEFAULT_SEMANTIC_VERSIONS_LIMIT,
                cursor: None,
                ..
            }]
        ));
    }
    let document =
        backend_library::SemanticSourceDocument::new(&source).expect("complete real source");
    let mut union = backend_library::SemanticSourceAssembly::new(reference.source.clone())
        .expect("exact captured union");
    loop {
        let range = backend_library::SemanticSourceRange {
            offset: union.offset().expect("ordered offset"),
            ..Default::default()
        };
        let chunk = document.chunk(&range).expect("native compact byte page");
        let mut server = ready(Fake {
            surface_reply: Some(SurfaceReply::SemanticSource(chunk.clone())),
            ..Fake::default()
        });
        let response = request(
            &mut server,
            "tools/call",
            &json!({"name":"backend.semantic_source","arguments":{"request":{"reference":reference,"range":range}}}),
        );
        assert_context_bounded(&response);
        assert!(
            wire_tokens(&response).expect("actual source whole envelope tokens")
                <= MCP_CL100K_TOKEN_LIMIT
        );
        assert_eq!(response["result"]["isError"], false, "{response}");
        let restored: backend_library::SemanticSourceChunk = serde_json::from_value(
            response["result"]["structuredContent"]["semantic_data"]["value"].clone(),
        )
        .expect("typed native range");
        assert_eq!(restored, chunk);
        union.append(&restored).expect("whole byte identity/order");
        if restored.complete {
            break;
        }
    }
    assert_eq!(
        union.finish().expect("complete digest/canonical admission"),
        source
    );
}

#[test]
fn archived_semantic_versions_token_admissible_source_retains_cli_projection() {
    // A separate complete typed source fixture; no entries are removed from
    // the actual 42-image capture exercised above to make this success fit.
    let operands: Value = serde_json::from_str(include_str!(
        "../../../../crates/library/fixtures/semantic-shape-read.json"
    ))
    .expect("complete selected source operand");
    let source: backend_library::SemanticVersionRecord =
        serde_json::from_value(operands["source"].clone()).expect("exact typed source");
    let reply = SurfaceReply::SemanticVersions(Box::new([source.clone()]));
    let answer = Answer::Product(Box::new(backend_present::product_view(&reply)));
    for detail in [Detail::Summary, Detail::Standard, Detail::Full] {
        let mut server = ready(Fake::default());
        let arguments = json!({"package":source.package.as_str(),"detail":detail.name()});
        let context = continuation_context(
            PROJECT,
            "backend.semantic_versions",
            arguments.as_object().expect("arguments"),
            detail,
        );
        let response = encoded_stdio_reply(codec::success(
            json!(9),
            server
                .rendered(
                    Answer::Product(Box::new(backend_present::product_view(&reply))),
                    detail,
                    &context,
                    CursorTarget::CliOption,
                    McpInvocation::new(
                        McpExposure::Direct,
                        "backend.semantic_versions",
                        arguments.as_object().expect("arguments"),
                    ),
                )
                .expect("archived typed display"),
        ));
        assert_context_bounded(&response);
        assert!(
            wire_tokens(&response).expect("actual whole-envelope tokens") <= MCP_CL100K_TOKEN_LIMIT
        );
        let result = &response["result"];
        assert_eq!(result["isError"], false, "{response}");
        assert_eq!(
            result["structuredContent"]["semantic_data"]["value"],
            json!([source])
        );
        let dto: backend_present::ProductDto =
            serde_json::from_value(result["structuredContent"].clone())
                .expect("actual public typed version decode");
        assert_eq!(
            dto.semantic_data,
            Some(backend_present::ProductSemanticData::Versions(Box::new([
                source.clone()
            ])))
        );
        let mut selected = operands.clone();
        selected["source"] = result["structuredContent"]["semantic_data"]["value"][0].clone();
        let selected: backend_library::SemanticShapeReadRequest =
            serde_json::from_value(selected).expect("exact source remains a usable shape operand");
        assert_eq!(
            selected.source(),
            &backend_library::SemanticShapeSource::Inline(source.clone())
        );
        let cli =
            backend_present::encode_answer(&answer, detail, None, DEFAULT_RESPONSE_BUDGET_BYTES)
                .expect("shared CLI encoder");
        let cli: Value = serde_json::from_slice(&cli.bytes).expect("CLI JSON");
        assert_eq!(result["structuredContent"], cli);
        assert_eq!(text_of(result), bounded_text(&markdown::answer(&answer)));
        assert!(
            server.product.surface_commands.is_empty(),
            "archived display creates no native owner call"
        );
    }
}

#[test]
fn semantic_versions_jsonrpc_keeps_true_oversize_refusals_atomic() {
    let packet: Value = serde_json::from_str(include_str!(
        "../../../../crates/present/fixtures/semantic-versions-semver-public.json"
    ))
    .expect("complete captured public source packet");
    let reply: SurfaceReply =
        serde_json::from_value(packet["surface"].clone()).expect("complete public source DTO");
    let SurfaceReply::SemanticVersions(records) = reply else {
        panic!("captured semantic versions")
    };
    // Repeating an actual display operand exercises only egress budgeting. It
    // does not assert that a compiler owner published duplicate generations.
    for detail in ["summary", "standard", "full"] {
        let mut server = ready(Fake::default());
        let display = SurfaceReply::SemanticVersions(
            vec![records[0].clone(), records[0].clone()].into_boxed_slice(),
        );
        let arguments = json!({"package":records[0].package.as_str(),"detail":detail});
        let detail = Detail::parse(detail).expect("detail");
        let context = continuation_context(
            PROJECT,
            "backend.semantic_versions",
            arguments.as_object().expect("arguments"),
            detail,
        );
        let response = encoded_stdio_reply(codec::success(
            json!(9),
            server
                .rendered(
                    Answer::Product(Box::new(backend_present::product_view(&display))),
                    detail,
                    &context,
                    CursorTarget::CliOption,
                    McpInvocation::new(
                        McpExposure::Direct,
                        "backend.semantic_versions",
                        arguments.as_object().expect("arguments"),
                    ),
                )
                .expect("atomic archived egress refusal"),
        ));
        assert_context_bounded(&response);
        assert_eq!(response["result"]["isError"], true);
        assert_eq!(
            response["result"]["structuredContent"]["cause"],
            "oversized"
        );
        assert!(
            response["result"]["structuredContent"]
                .get("semantic_data")
                .is_none()
        );
    }
}

#[test]
fn semantic_shapes_jsonrpc_refuses_oversized_view_without_dropping_required_facts() {
    // This is an untrusted egress fixture at the surface budget seam, not a
    // synthetic certificate-admitted compiler product or a corpus pass.
    let mut view: Value = serde_json::from_str(include_str!(
        "../../../../crates/library/fixtures/semantic-shape-egress-view.json"
    ))
    .expect("view");
    let mut operands: Value = serde_json::from_str(include_str!(
        "../../../../crates/library/fixtures/semantic-shape-read.json"
    ))
    .expect("operands");
    // Every text field remains under its public bound. Thirty-two display-only
    // closure-unavailable entries retain their exact source operand and exceed
    // the MCP packet budget without fabricating available compiler facts.
    let package = format!("/abs/{}", "x".repeat(3500));
    view["source"]["package"]["value"] = json!(package);
    operands["source"]["package"]["value"] = json!(package);
    operands["source"]["selected_source_frontier"]["package"]["value"] = json!(package);
    view["max_bytes"] = json!(262144);
    operands["max_bytes"] = json!(262144);
    let source: backend_library::SemanticVersionRecord =
        serde_json::from_value(operands["source"].clone()).expect("bounded source operand");
    let origin = backend_library::SemanticShapeSourceOrigin {
        source: backend_library::SemanticShapeSelection::from_selected(&source)
            .expect("selected fixture source"),
        selection_root: [8; 32],
        semantic_image_bytes: backend_library::SemanticImagePayloadBytes::new(4096)
            .expect("bounded fixture extent"),
        image: None,
    };
    let commitment = backend_library::semantic_shape_source_key(&origin);
    let commitment_hex: String = commitment
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let origin_view = json!({
        "source": view["source"], "selection_root": origin.selection_root,
        "semantic_image_bytes": 4096, "image": null,
        "source_commitment": commitment_hex,
    });
    let entries: Vec<_> = (1u8..=32)
        .map(|id| {
            json!({
                "symbol":{"kind":{"kind":"selected"},"id":format!("{id:02x}").repeat(32)},
                "identity":null,"origin":origin_view,
                "fact":{"state":"unavailable","data":"missing_image_fact"},
            })
        })
        .collect();
    view["batch"]["entries"] = json!(entries);
    operands["symbols"] = json!((1u8..=32).map(|id| [id; 32]).collect::<Vec<_>>());
    let export: backend_library::SemanticShapeExport =
        serde_json::from_value(view).expect("bounded view");
    for detail in ["summary", "full"] {
        let mut server = ready(Fake {
            surface_reply: Some(SurfaceReply::SemanticShapes(export.clone())),
            ..Fake::default()
        });
        let result = call(
            &mut server,
            "backend.semantic_shapes",
            &json!({"request":operands,"detail":detail}),
        );
        assert_context_bounded(&result);
        assert_eq!(result["isError"], true);
        assert_eq!(result["structuredContent"]["cause"], "oversized");
        assert!(result["structuredContent"].get("semantic_data").is_none());
    }
}

fn text_of(result: &Value) -> String {
    result["content"][0]["text"]
        .as_str()
        .expect("a tool result carries one text block")
        .to_owned()
}

fn tool_named<'a>(tools: &'a Value, name: &str) -> &'a Value {
    tools
        .as_array()
        .expect("tools is an array")
        .iter()
        .find(|tool| tool["name"] == name)
        .unwrap_or_else(|| panic!("no tool named `{name}`"))
}

fn assert_context_bounded(response: &Value) {
    let bytes = serde_json::to_vec(response)
        .expect("JSON-RPC response serializes")
        .len()
        .saturating_add(1);
    assert!(
        bytes <= DEFAULT_RESPONSE_BUDGET_BYTES,
        "response is {} bytes, above the {} byte context budget: {response}",
        bytes,
        DEFAULT_RESPONSE_BUDGET_BYTES
    );
}

fn assert_stdio_line_accounting(line: &str) -> Value {
    assert!(line.ends_with('\n'), "stdio owes a complete response line");
    assert_eq!(
        line.as_bytes()
            .iter()
            .filter(|byte| **byte == b'\n')
            .count(),
        1
    );
    assert!(line.len() <= DEFAULT_RESPONSE_BUDGET_BYTES);
    let reply: Value = serde_json::from_str(line).expect("complete stdio JSON response");
    assert_eq!(reply["jsonrpc"], "2.0");
    let budget = if reply.get("result").is_some() {
        &reply["result"]["_meta"]["backend/wireBudget"]
    } else {
        &reply["error"]["data"]["_meta"]["backend/wireBudget"]
    };
    assert_eq!(budget["bytes"], line.len());
    assert_eq!(budget["estimatedTokens"], estimate_tokens(line.len()));
    assert_eq!(budget["bytesPerToken"], ESTIMATED_BYTES_PER_TOKEN);
    assert_eq!(budget["limitBytes"], DEFAULT_RESPONSE_BUDGET_BYTES);
    assert_eq!(budget["scope"], "complete_jsonrpc_line");
    reply
}

fn paging_catalog(count: usize) -> backend_library::Library {
    let mut rows = vec![package_row(), module_row()];
    rows.extend((0..count).map(|index| {
        let coordinate = format!("{MODULE}:{}::Session_{index:03}", index + 2);
        Row::in_package(
            RowId::Symbol(symbol_key(&coordinate)),
            basis(),
            package_key(PROJECT),
            coordinate,
        )
        .with_parent(symbol_key(MODULE))
    }));
    // This is a genuine incomplete catalog projection; no compiler or
    // complete-coverage authority is asserted by this paging fixture.
    let view = root(rows);
    let cursor = backend_library::Cursor::for_view_root(&view);
    backend_library::Library::from_view(view, cursor).expect("catalog projection")
}

fn selected_search_page(
    catalog: &backend_library::Library,
    query: &backend_library::Query,
) -> Result<ViewSnapshot, backend_library::LibraryError> {
    // Use the same producer projection seam as the application search
    // service: canonical row storage and relevance order remain distinct.
    let prefix = catalog.search_ranked(&backend_library::Query::new(
        query.text(),
        catalog.revision_root(),
        backend_library::QueryLimit::new(backend_library::QueryLimit::MAX).expect("bounded prefix"),
    ))?;
    catalog.search_from_ranked_ids(query, prefix.order())
}

#[test]
fn default_collection_pages_keep_owner_order_exactly_once_and_reject_new_snapshots() {
    for tool in ["backend.search", "backend.resolve", "backend.graph"] {
        let catalog = paging_catalog(61);
        let credit = backend_library::QueryLimit::new(200).expect("bounded oracle page");
        let snapshot = match tool {
            "backend.search" => selected_search_page(
                &catalog,
                &backend_library::Query::new("Session", catalog.revision_root(), credit),
            )
            .expect("complete selected search"),
            "backend.resolve" => catalog
                .names(&backend_library::NameQuery::new(
                    "Session",
                    catalog.revision_root(),
                    credit,
                ))
                .expect("complete selected names"),
            _ => {
                catalog
                    .graph_page(
                        symbol_key(MODULE),
                        backend_library::PageRequest::new(catalog.revision_root(), credit),
                    )
                    .expect("complete selected neighborhood")
                    .snapshot
            }
        };
        let mut expected = record_list("selected", &snapshot)
            .records()
            .iter()
            .map(|record| record.identity().coordinate().as_str().to_owned())
            .collect::<Vec<_>>();
        if tool == "backend.graph" {
            // Neighborhood snapshots sort identities within each selected
            // page. The root/parent prefix precedes children in owner page
            // selection, so compare the actual owner order at this credit.
            let membership = expected
                .iter()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>();
            expected.clear();
            let mut cursor = None;
            loop {
                let mut page = backend_library::PageRequest::new(
                    catalog.revision_root(),
                    backend_library::QueryLimit::new(backend_present::DEFAULT_LIMIT)
                        .expect("default credit"),
                );
                if let Some(next) = cursor {
                    page = page.with_continuation(next);
                }
                let page = catalog
                    .graph_page(symbol_key(MODULE), page)
                    .expect("owner page");
                expected.extend(
                    record_list("selected", &page.snapshot)
                        .records()
                        .iter()
                        .map(|record| record.identity().coordinate().as_str().to_owned()),
                );
                let PageTerminal::More(next) = page.terminal else {
                    break;
                };
                cursor = Some(next);
            }
            assert_eq!(
                expected
                    .iter()
                    .cloned()
                    .collect::<std::collections::BTreeSet<_>>(),
                membership
            );
        }
        let mut server = ready(Fake {
            page_catalog: Some(catalog),
            document_matches_request: tool == "backend.search",
            ..Fake::default()
        });
        let mut arguments = if tool == "backend.graph" {
            json!({"coordinate": MODULE})
        } else {
            json!({"query": "Session"})
        };
        let mut seen = Vec::new();
        let mut first_cursor = None;
        let mut first_text_coordinate = None;
        for _ in 0..10 {
            let response = request(
                &mut server,
                "tools/call",
                &json!({"name": tool, "arguments": arguments}),
            );
            assert_context_bounded(&response);
            let result = &response["result"];
            assert_eq!(result["isError"], false, "{tool}: {response}");
            assert_eq!(result["structuredContent"]["detail"], "summary");
            let records = result["structuredContent"]["records"]
                .as_array()
                .expect("complete typed rows");
            assert!(records.len() <= usize::from(backend_present::DEFAULT_LIMIT));
            let coordinates = records
                .iter()
                .map(|row| {
                    let name = row["identity"]["name"]
                        .as_str()
                        .expect("typed identity name");
                    let coordinate = row["identity"]["coordinate"]
                        .as_str()
                        .expect("exact coordinate");
                    (name, coordinate)
                })
                .collect::<Vec<_>>();
            seen.extend(
                coordinates
                    .iter()
                    .map(|(_, coordinate)| (*coordinate).to_owned()),
            );
            let preview = text_of(result);
            let text_coordinates = summary_coordinates(&preview);
            assert_eq!(
                text_coordinates,
                coordinates
                    .iter()
                    .take(text_coordinates.len())
                    .map(|(_, coordinate)| (*coordinate).to_owned())
                    .collect::<Vec<_>>(),
                "text rows preserve the complete owner-order coordinate prefix: {tool}"
            );
            assert_eq!(
                text_coordinates.len(),
                records.len(),
                "ordinary bounded fixture pages fit as complete text rows: {tool}"
            );
            assert!(!text_coordinates.is_empty(), "{tool}: {preview}");
            for (name, coordinate) in &coordinates[..text_coordinates.len()] {
                assert!(preview.contains(name), "{tool}: {preview}");
                assert!(
                    preview.contains(&format!("`{coordinate}`")),
                    "{tool}: {preview}"
                );
            }
            assert!(preview.len() <= backend_present::MAX_PREVIEW_TEXT_BYTES);
            assert_eq!(
                estimate_tokens(preview.len()),
                preview
                    .len()
                    .div_ceil(backend_present::ESTIMATED_BYTES_PER_TOKEN),
                "stable preview cost estimate"
            );
            first_text_coordinate.get_or_insert_with(|| text_coordinates[0].clone());
            if let Some(cursor) = result["structuredContent"]["nextCursor"].as_str() {
                assert_eq!(result["structuredContent"]["more"], true);
                assert!(preview.contains(&format!("Continue `{tool}` with the same arguments")));
                assert!(
                    preview.contains("copy `structuredContent.nextCursor` to `arguments.cursor`")
                );
                assert!(!preview.contains(&format!("`{cursor}`")));
                assert!(!preview.contains("raise `limit`"), "{preview}");
                first_cursor.get_or_insert_with(|| cursor.to_owned());
                arguments["cursor"] = json!(cursor);
            } else {
                break;
            }
        }
        assert_eq!(
            seen, expected,
            "{tool} preserves owner order, no skipped/duplicate rows"
        );
        assert_eq!(
            server.product.page_limits,
            vec![backend_present::DEFAULT_LIMIT; 3]
        );
        let first_cursor = first_cursor.expect("ordinary default continues");

        if tool == "backend.search" {
            // The caller selects from the human-readable block alone; the
            // protocol-owned cursor remains in structuredContent as before.
            let selected = first_text_coordinate
                .as_deref()
                .expect("text preview includes a selectable coordinate");
            let opened = call(
                &mut server,
                "backend.document",
                &json!({"coordinate":selected,"detail":"summary"}),
            );
            assert_context_bounded(&opened);
            assert_eq!(opened["isError"], false, "{opened}");
            assert_eq!(
                opened["structuredContent"]["identity"]["coordinate"], selected,
                "the selected exact coordinate is the opened document"
            );
            assert_eq!(
                server
                    .product
                    .document_coordinates
                    .last()
                    .map(String::as_str),
                Some(selected),
                "the exact selected text operand reaches backend.document"
            );
        }

        // The same query spelling and MAC cannot make an old projection
        // current when the underlying catalog changes.
        server.product.page_catalog = Some(paging_catalog(62));
        arguments["cursor"] = json!(first_cursor);
        let stale = request(
            &mut server,
            "tools/call",
            &json!({"name": tool, "arguments": arguments}),
        );
        let fault = tool_failure(&stale, "cursor-mismatch");
        assert_eq!(fault["cause"], "moved");
        assert!(text_of(&stale["result"]).contains("restart the query"));
    }
}

fn summary_coordinates(preview: &str) -> Vec<String> {
    let lines = preview.lines().collect::<Vec<_>>();
    let mut coordinates = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if line.starts_with("• ") {
            let coordinate_line = lines
                .get(index + 1)
                .expect("every displayed summary label has its coordinate line");
            coordinates.push(
                test_code_span(coordinate_line, "  ")
                    .expect("every displayed summary coordinate is one complete code span"),
            );
        }
        if line.starts_with("  ") {
            assert!(
                index > 0 && lines[index - 1].starts_with("• "),
                "a summary coordinate cannot appear without its label: {preview}"
            );
        }
    }
    coordinates
}

fn test_code_span(line: &str, prefix: &str) -> Option<String> {
    let span = line.strip_prefix(prefix)?;
    let delimiter = span.bytes().take_while(|byte| *byte == b'`').count();
    if delimiter == 0 {
        return None;
    }
    let marker = "`".repeat(delimiter);
    let content_start = delimiter;
    let closing_start = span[content_start..].find(&marker)? + content_start;
    if !span[closing_start + delimiter..].is_empty() {
        return None;
    }
    let mut content = &span[content_start..closing_start];
    if content.starts_with(' ')
        && content.ends_with(' ')
        && content.chars().any(|character| character != ' ')
    {
        content = &content[1..content.len() - 1];
    }
    Some(content.to_owned())
}

fn summary_rows_fixture(count: usize) -> (String, Vec<String>) {
    let mut text = String::from("~lanes exact\n");
    let mut coordinates = Vec::with_capacity(count);
    for index in 0..count {
        let name = format!("symbol_{index:03}");
        let coordinate = format!("/workspace/project::src/lib.rs:{}::{name}", index + 1);
        text.push_str(&format!("• `{name}` · function · rust\n  `{coordinate}`\n"));
        coordinates.push(coordinate);
    }
    text.push_str(&format!(
        "{count} row(s) on this page; showing {count} exact coordinate(s); 0 omitted from text preview. Full page: `structuredContent.records`.\n"
    ));
    (text, coordinates)
}

#[test]
fn every_interior_byte_of_a_rendered_backtick_unicode_row_backs_up_to_its_start() {
    let name = " `λ` ";
    let coordinate = format!("{PROJECT}::src/lib.rs:7::{name}");
    let row = Row::new(
        RowId::Symbol(symbol_key(&coordinate)),
        basis(),
        coordinate.clone(),
    );
    let answer = backend_present::Answer::Records(Box::new(backend_present::RecordList::new(
        name,
        backend_present::CoverageLine::new(&[Coverage::Complete], Some(1)),
        vec![backend_present::Record::from_row(&row)],
    )));
    let preview = backend_present::markdown::summary_with_cursor(
        &answer,
        None,
        ContinuationTarget::McpTool("backend.search"),
    );
    let row_start = preview.find("• ").expect("actual rendered row");
    let label_end = preview[row_start..]
        .find('\n')
        .map(|offset| row_start + offset)
        .expect("label line end");
    let coordinate_start = label_end + 1;
    let coordinate_end = preview[coordinate_start..]
        .find('\n')
        .map(|offset| coordinate_start + offset)
        .expect("coordinate line end");
    let row_end = coordinate_end + 1;

    assert!(preview.contains("• ``  `λ`  `` · rust"), "{preview}");
    assert_eq!(summary_coordinates(&preview), vec![coordinate]);
    assert!(preview.contains("1 row(s) on this page; showing 1 exact coordinate(s); 0 omitted"));
    for limit in row_start + 1..row_end {
        assert_eq!(
            readable_prefix_end(&preview, limit),
            row_start,
            "byte offset {limit} must not retain a partial label or operand"
        );
    }
    assert_eq!(readable_prefix_end(&preview, row_start), row_start);
    assert_eq!(readable_prefix_end(&preview, row_end), row_end);
}

#[test]
fn result_budget_shortening_preserves_complete_summary_coordinate_rows() {
    let (text, coordinates) = summary_rows_fixture(100);
    let structured = json!({
        "records": " ".repeat(41_000),
        "nextCursor": "owner-cursor-remains-structured",
        "continuation_call":{"name":"backend.call","arguments":{"name":"backend.search","arguments":{
            "query":"owner scope \" literal", "limit":3, "cursor":"owner-cursor-remains-structured"}}}
    });
    let result = tool_result(&text, structured.clone(), false);
    assert_eq!(result["structuredContent"], structured);
    assert_eq!(result["isError"], false);
    let preview = text_of(&result);
    assert!(preview.ends_with(PREVIEW_MARKER));
    assert!(
        preview.contains("when present, copy structuredContent.continuation_call unchanged"),
        "budget shortening retains executable next-page discovery: {preview}"
    );
    let shown = summary_coordinates(&preview);
    assert!(!shown.is_empty());
    assert!(shown.len() < coordinates.len());
    assert_eq!(shown, coordinates[..shown.len()]);
    assert!(preview.contains(&format!(
        "100 row(s) on this page; showing {} exact coordinate(s); {} omitted",
        shown.len(),
        100 - shown.len()
    )));
    assert!(preview.contains("\n\n… readable preview shortened"));
    assert!(!preview.contains("owner-cursor-remains-structured"));
}

#[test]
fn final_rpc_budget_shortening_preserves_complete_summary_coordinate_rows() {
    let (text, coordinates) = summary_rows_fixture(100);
    let structured = json!({
        "records": " ".repeat(34_000),
        "nextCursor": "owner-cursor-remains-structured",
        "continuation_call":{"name":"backend.call","arguments":{"name":"backend.search","arguments":{
            "query":"owner scope \" literal", "limit":3, "cursor":"owner-cursor-remains-structured"}}}
    });
    let result = tool_result(&text, structured.clone(), false);
    assert_eq!(
        text_of(&result),
        text.as_str(),
        "the final envelope owns this shortening"
    );

    let response = encoded_stdio_reply(success(
        json!("request-".to_owned() + &"r".repeat(8_000)),
        result,
    ));
    assert_context_bounded(&response);
    assert_eq!(response["result"]["structuredContent"], structured);
    let preview = text_of(&response["result"]);
    assert!(preview.ends_with(PREVIEW_MARKER));
    assert!(
        preview.contains("when present, copy structuredContent.continuation_call unchanged"),
        "budget shortening retains executable next-page discovery: {preview}"
    );
    let shown = summary_coordinates(&preview);
    assert!(!shown.is_empty());
    assert!(shown.len() < coordinates.len());
    assert_eq!(shown, coordinates[..shown.len()]);
    assert!(preview.contains(&format!(
        "100 row(s) on this page; showing {} exact coordinate(s); {} omitted",
        shown.len(),
        100 - shown.len()
    )));
    assert!(!preview.contains("owner-cursor-remains-structured"));
}

#[test]
fn combined_budget_shortens_only_the_readable_preview_with_explicit_provenance() {
    // Egress-only values exercise escaped-byte measurement and UTF-8
    // boundaries. They claim no owner-admitted source or compiler evidence.
    let cursor = "mcp1-complete-signed-cursor";
    let structured = json!({"records": [{"coordinate": "λאב".repeat(5000)}], "coverage": "unavailable", "nextCursor": cursor});
    let text = "\"\\\nλאב".repeat(3000);
    let result = tool_result(&text, structured.clone(), false);
    assert_eq!(result["isError"], false);
    assert_eq!(result["structuredContent"], structured);
    let preview = text_of(&result);
    assert!(preview.contains("readable preview shortened"));
    assert!(preview.contains("complete page and any nextCursor"));
    assert!(text.starts_with(preview.split("\n\n…").next().expect("UTF-8 prefix")));
    assert_context_bounded(&success(json!(17), result));

    let too_large = json!({"records": ["x".repeat(MCP_RESULT_BUDGET_BYTES)]});
    let refused = tool_result(&text, too_large, false);
    assert_eq!(refused["isError"], true);
    assert_eq!(refused["structuredContent"]["cause"], "oversized");
    assert!(refused["structuredContent"].get("records").is_none());
}

fn setup_compiler_failure() -> backend_library::PackageCompilerFailure {
    use backend_library::interface::{CompilerTerminal, SourceAuthority};
    use backend_semantic::vocabulary::{Language, NativeTool, Stage};
    use backend_version::{ContentId, SourceFactDomain};

    let bytes = b"export function welcome(): string { return 'hello'; }";
    backend_library::PackageCompilerFailure::from_package_terminal(
        "src/main.ts",
        &CompilerTerminal::Toolchain {
            source: SourceAuthority {
                identity: ContentId::<SourceFactDomain>::from_canonical_bytes(bytes),
                byte_len: u32::try_from(bytes.len()).expect("small source"),
            },
            language: Language::TypeScript,
            stage: Stage::LowerIr,
            selected: NativeTool::TypeScriptCompiler,
            configured: None,
        },
    )
    .expect("valid compiler terminal")
    .expect("setup refusal projects")
}

fn tool_failure<'a>(response: &'a Value, slug: &str) -> &'a Value {
    assert!(
        response.get("error").is_none(),
        "known tool failure is a result: {response}"
    );
    assert_eq!(response["result"]["isError"], true, "{response}");
    let structured = &response["result"]["structuredContent"];
    assert_eq!(structured["answer"], "fault", "{response}");
    assert_eq!(structured["slug"], slug, "{response}");
    assert_context_bounded(response);
    structured
}
fn compiler_error_routes() -> Vec<(&'static str, Value, &'static str)> {
    let command = SurfaceCommand::IndexStart {
        package: PackageReference::parse(PROJECT).expect("project reference"),
        execution_intent: CompileExecutionIntent::Interactive,
    };
    vec![
        (
            INDEX_START_TOOL,
            json!({"package": PROJECT, "detail": "full"}),
            PROJECT,
        ),
        (
            INDEX_PROGRESS_TOOL,
            json!({"ticket": ticket_value(&index_job_ticket()), "detail": "full"}),
            "pkg:cargo/serde@1.0.228",
        ),
        (
            INDEX_CANCEL_TOOL,
            json!({"ticket": ticket_value(&index_job_ticket()), "detail": "full"}),
            "pkg:cargo/serde@1.0.228",
        ),
        (
            SURFACE_TOOL,
            json!({"command": command, "detail": "full"}),
            PROJECT,
        ),
    ]
}

fn compiler_client_error(failure: &backend_library::PackageCompilerFailure) -> ClientError {
    ClientError::CommandFailed(backend_library::CommandFailure::CompilerRefused {
        detail: "RAW SOURCE DIAGNOSTIC AND LEGACY JSON".repeat(2_000),
        failure: failure.clone(),
    })
}

fn adapter_error_routes() -> Vec<(&'static str, Value, &'static str, &'static str)> {
    vec![
        (
            "tools/call",
            json!({"name": "backend.graph", "arguments": {"coordinate": DECLARATION, "limit": 1}}),
            DECLARATION,
            "graph_page",
        ),
        (
            "prompts/get",
            json!({"name": "backend.explore", "arguments": {"query": "welcome"}}),
            PROJECT,
            "revision",
        ),
        (
            "tools/call",
            json!({"name": QUERY_TOOL, "arguments": {"query": "{ Declaration { coordinate @output } }", "limit": 1}}),
            PROJECT,
            "encode_continuation",
        ),
    ]
}

#[test]
fn adapter_errors_preserve_typed_compiler_facts_at_exact_boundaries() {
    let failure = setup_compiler_failure();
    for (method, params, operand, boundary) in adapter_error_routes() {
        let mut server = ready(Fake {
            adapter_error: Some(compiler_client_error(&failure)),
            graph_continue: true,
            ..Fake::default()
        });
        let response = request(&mut server, method, &params);
        let (structured, detail) = if method == "tools/call" {
            (
                tool_failure(&response, "compiler-refused"),
                text_of(&response["result"]),
            )
        } else {
            let data = &response["error"]["data"];
            assert_eq!(data["kind"], "compiler-refused", "{boundary}: {response}");
            (
                &data["structuredContent"],
                data["detail"].as_str().expect("RPC explanation").to_owned(),
            )
        };
        assert_eq!(server.product.adapter_boundary, Some(boundary));
        assert_eq!(
            structured["compiler_failure"],
            serde_json::to_value(&failure).expect("exact facts")
        );
        assert_eq!(structured["operand"], operand);
        assert_eq!(
            structured["compiler_tool_requirement"]["configuration_variable"],
            "NUDOX_TSC"
        );
        assert!(detail.contains("src/main.ts: setup/toolchain_configuration_mismatch"));
        assert!(!detail.contains("RAW SOURCE DIAGNOSTIC"));
        assert!(!detail.contains("content:"));
        assert!(detail.len() < 500);
        assert_context_bounded(&response);
    }
}

#[test]
fn adapter_errors_keep_valid_json_and_coordinate_protocol_strings_unproven() {
    let failure = setup_compiler_failure();
    let encoded = failure
        .encode_bounded_json()
        .expect("bounded compiler JSON");
    assert_eq!(
        backend_library::PackageCompilerFailure::decode_bounded_json(&encoded)
            .expect("valid compiler JSON"),
        failure
    );
    for message in [
        String::from_utf8(encoded).expect("compiler JSON is UTF-8"),
        "/abs/trap::src/hidden.ts:12::Secret: library record not found".to_owned(),
    ] {
        for (method, params, operand, boundary) in adapter_error_routes() {
            let mut server = ready(Fake {
                adapter_error: Some(ClientError::Protocol(message.clone())),
                graph_continue: true,
                ..Fake::default()
            });
            let response = request(&mut server, method, &params);
            let data = &response["error"]["data"];
            let structured = &data["structuredContent"];
            assert_eq!(server.product.adapter_boundary, Some(boundary));
            assert_eq!(data["kind"], "protocol", "{boundary}: {response}");
            assert_eq!(structured["cause"], "unproven");
            assert_eq!(structured["operand"], operand);
            assert!(structured.get("compiler_failure").is_none());
            assert!(structured.get("compiler_tool_requirement").is_none());
            assert_context_bounded(&response);
        }
    }
}

#[test]
fn adapter_errors_keep_continuation_transport_refusal_separate_from_compiler_facts() {
    let (method, params, operand, boundary) =
        adapter_error_routes().pop().expect("continuation route");
    let mut server = ready(Fake {
        adapter_error: Some(ClientError::Transport(
            backend_replication::ReplicationError::MessageTooLarge,
        )),
        graph_continue: true,
        ..Fake::default()
    });
    let response = request(&mut server, method, &params);
    let structured = tool_failure(&response, "transport");
    assert_eq!(server.product.adapter_boundary, Some(boundary));
    assert_eq!(server.product.graph_query_calls, 1);
    assert_eq!(structured["cause"], "oversized");
    assert_eq!(structured["operand"], operand);
    assert!(structured.get("compiler_failure").is_none());
    assert!(structured.get("compiler_tool_requirement").is_none());
    assert_context_bounded(&response);
}

#[test]
fn surface_errors_preserve_typed_compiler_facts_across_all_job_routes() {
    let failure = setup_compiler_failure();
    for (tool, arguments, operand) in compiler_error_routes() {
        let mut server = ready(Fake {
            surface_error: Some(compiler_client_error(&failure)),
            ..Fake::default()
        });
        let response = request(
            &mut server,
            "tools/call",
            &json!({"name": tool, "arguments": arguments}),
        );
        let structured = tool_failure(&response, "compiler-refused");
        assert_eq!(
            structured["compiler_failure"],
            serde_json::to_value(&failure).expect("exact facts")
        );
        assert_eq!(structured["cause"], "refused");
        assert_eq!(structured["operand"], operand);
        assert_eq!(
            structured["compiler_tool_requirement"]["configuration_variable"],
            "NUDOX_TSC"
        );
        assert_eq!(
            structured["compiler_tool_requirement"]["configuration_required"],
            true
        );
        let detail = text_of(&response["result"]);
        assert!(detail.contains("src/main.ts: setup/toolchain_configuration_mismatch"));
        assert!(detail.contains("Set NUDOX_TSC to an absolute path"));
        assert!(!detail.contains("RAW SOURCE DIAGNOSTIC"));
        assert!(!detail.contains("content:"));
        assert!(!detail.contains("facts="));
        assert!(detail.len() < 500);
        assert_eq!(server.product.surface_commands.len(), 1);
        assert_context_bounded(&response);
    }
}

#[test]
fn surface_errors_keep_valid_compiler_json_and_coordinates_as_unproven_protocol() {
    let failure = setup_compiler_failure();
    let encoded = failure
        .encode_bounded_json()
        .expect("bounded compiler JSON");
    assert_eq!(
        backend_library::PackageCompilerFailure::decode_bounded_json(&encoded)
            .expect("valid compiler JSON"),
        failure
    );
    for message in [
        String::from_utf8(encoded).expect("compiler JSON is UTF-8"),
        "/abs/trap::src/hidden.ts:12::Secret: library record not found".to_owned(),
    ] {
        for (tool, arguments, operand) in compiler_error_routes() {
            let mut server = ready(Fake {
                surface_error: Some(ClientError::Protocol(message.clone())),
                ..Fake::default()
            });
            let response = request(
                &mut server,
                "tools/call",
                &json!({"name": tool, "arguments": arguments}),
            );
            let data = &response["error"]["data"];
            let structured = &data["structuredContent"];
            assert_eq!(data["kind"], "protocol", "{tool}: {response}");
            assert_eq!(structured["cause"], "unproven");
            assert_eq!(structured["operand"], operand);
            assert!(structured.get("compiler_failure").is_none());
            assert!(structured.get("compiler_tool_requirement").is_none());
            assert_context_bounded(&response);
        }
    }
}

// ---------------------------------------------------------------------------
// registry completeness
// ---------------------------------------------------------------------------

#[test]
fn every_registry_row_is_reachable_as_exactly_one_tool() {
    let mut server = ready(Fake::default());
    let listed = request(&mut server, "tools/list", &json!({}));
    let tools = &listed["result"]["tools"];
    const SESSION: &[&str] = &[
        "packages",
        "add",
        "remove",
        "health",
        "outline",
        "search",
        "resolve",
        "show",
        "source",
        "read",
        "references",
        "graph",
        "package",
        "index-search",
    ];
    for name in SESSION {
        let spec = COMMANDS
            .iter()
            .find(|spec| spec.name == *name)
            .unwrap_or_else(|| panic!("session row `{name}` left the registry"));
        let grammar = grammar_for(spec.name)
            .unwrap_or_else(|| panic!("registry row `{}` has no grammar", spec.name));
        let tool = tool_named(tools, grammar.tool());
        assert_eq!(tool["title"], spec.title, "`{}` title", spec.name);
        let description = tool["description"].as_str().unwrap_or_default();
        assert!(
            description.starts_with(spec.description),
            "`{}` must lead with its registry sentence: {description}",
            spec.name
        );
        assert!(
            description.len() > spec.description.len() + 20,
            "`{}` must also say when to reach for it",
            spec.name
        );
        assert_eq!(
            tool["_meta"]["backend/domain"],
            domain_name(spec.domain),
            "`{}` domain",
            spec.name
        );
        assert_eq!(
            tool["annotations"]["readOnlyHint"],
            Value::Bool(!grammar.is_write()),
            "`{}` read-only hint",
            spec.name
        );
        let required = tool["inputSchema"]["required"]
            .as_array()
            .expect("required is an array");
        let mut expected = grammar
            .positional()
            .iter()
            .filter(|argument| argument.is_required() || grammar.tool() == "backend.index")
            .map(|argument| Value::String(argument.name().to_owned()))
            .collect::<Vec<_>>();
        if grammar.tool() == "backend.index" {
            expected = vec![Value::String("path".to_owned())];
        }
        assert_eq!(required, &expected, "`{}` required operands", spec.name);
        for argument in grammar.positional().iter().chain(grammar.options()) {
            assert!(
                !tool["inputSchema"]["properties"][argument.json_name()].is_null(),
                "`{}` omits operand `{}`",
                spec.name,
                argument.name()
            );
        }
    }
    let names = tools
        .as_array()
        .expect("tools is an array")
        .iter()
        .map(|tool| tool["name"].as_str().unwrap_or_default())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec![
            "backend.packages",
            "backend.index",
            "backend.remove",
            "backend.status",
            "backend.outline",
            "backend.search",
            "backend.resolve",
            "backend.document",
            "backend.source",
            "backend.read",
            "backend.references",
            "backend.graph",
            "backend.package",
            "backend.index_search",
            "backend.index_start",
            "backend.index_progress",
            "backend.index_cancel",
            "backend.call",
        ]
    );
}

#[test]
fn owner_index_job_tools_advertise_exact_tickets_and_immediate_progress() {
    let mut server = ready(Fake::default());
    let listed = request(&mut server, "tools/list", &json!({}));
    let tools = &listed["result"]["tools"];

    let start = tool_named(tools, INDEX_START_TOOL);
    assert_eq!(start["inputSchema"]["required"], json!(["package"]));
    assert_eq!(
        start["inputSchema"]["properties"]["execution_intent"]["enum"],
        json!(["interactive", "background"])
    );
    assert_eq!(start["annotations"]["readOnlyHint"], false);

    let progress = tool_named(tools, INDEX_PROGRESS_TOOL);
    assert_eq!(
        progress["inputSchema"]["oneOf"][0]["required"],
        json!(["ticket"])
    );
    assert_eq!(
        progress["inputSchema"]["oneOf"][1]["required"],
        json!(["operation_key"])
    );
    assert_eq!(
        progress["inputSchema"]["properties"]["ticket"]["oneOf"][0]["properties"]["owner_epoch"]["minItems"],
        16
    );
    assert_eq!(
        progress["inputSchema"]["properties"]["after_sequence"]["default"],
        0
    );
    assert_eq!(
        progress["inputSchema"]["properties"]["compact_receipt"]["default"],
        false
    );
    let output_surfaces = progress["outputSchema"]["properties"]["surface"]["oneOf"]
        .as_array()
        .expect("receipt output alternatives");
    let legacy_surface_schema = &output_surfaces[0];
    let compact_surface_schema = &output_surfaces[1];
    let compact_surface = json!({
        "result": "index-progress",
        "data": {"receipt_pointer": "#/surface/index_job/value"},
        "index_job": {"kind": "progress", "value": {"state": "pending"}}
    });
    let raw_surface = json!({
        "result": "index-progress",
        "data": {"state": "pending"},
        "index_job": {"kind": "progress", "value": {"state": "pending"}}
    });
    assert!(compact_surface_schema_matches(
        compact_surface_schema,
        &compact_surface
    ));
    assert!(legacy_pointer_exclusion_matches(
        legacy_surface_schema,
        &compact_surface
    ));
    assert!(!legacy_pointer_exclusion_matches(
        legacy_surface_schema,
        &raw_surface
    ));
    assert_eq!(
        legacy_surface_schema["properties"]["data"]["type"],
        "object",
        "the default raw data schema remains intact"
    );
    assert_eq!(progress["annotations"]["readOnlyHint"], true);
    let wait = &progress["inputSchema"]["properties"]["wait_millis"];
    assert_eq!(wait["maximum"], 10000);
    assert!(
        wait.get("default").is_none(),
        "a client applying defaults must not turn a bare key into an invalid wait"
    );
    assert!(
        wait["description"]
            .as_str()
            .expect("conditional default guidance")
            .contains("when after_observation is supplied")
    );
    let first_use = tool_named(tools, "backend.index");
    for tool in [
        first_use,
        start,
        progress,
        tool_named(tools, INDEX_CANCEL_TOOL),
    ] {
        assert_eq!(
            tool["outputSchema"]["properties"]["index_poll"]["required"],
            json!(["call"])
        );
        assert_eq!(
            tool["outputSchema"]["properties"]["index_poll"]["properties"]["call"]["properties"]["name"]
                ["const"],
            "backend.index_progress"
        );
        assert_eq!(
            tool["outputSchema"]["properties"]["index_poll"],
            first_use["outputSchema"]["properties"]["index_poll"]
        );
    }
    assert_eq!(first_use["outputSchema"]["additionalProperties"], true);
    assert_eq!(progress["outputSchema"]["additionalProperties"], false);
    assert!(
        tool_named(tools, "backend.search")["outputSchema"]["properties"]
            .get("index_poll")
            .is_none()
    );
    assert_eq!(
        progress["outputSchema"]["properties"]["surface"]["oneOf"][0]["required"],
        json!(["result", "data"])
    );

    let cancel = tool_named(tools, INDEX_CANCEL_TOOL);
    assert_eq!(cancel["inputSchema"]["required"], json!(["ticket"]));
    assert_eq!(cancel["annotations"]["readOnlyHint"], false);
    assert!(
        tools
            .as_array()
            .expect("tools array")
            .iter()
            .all(|tool| tool["name"] != "backend.index_await")
    );
    assert!(
        progress["description"]
            .as_str()
            .unwrap_or_default()
            .contains("unknown-after-restart")
    );
    assert!(
        cancel["description"]
            .as_str()
            .unwrap_or_default()
            .contains("not terminal")
    );
}

fn compact_surface_schema_matches(schema: &Value, surface: &Value) -> bool {
    schema["properties"]["result"]["const"] == surface["result"]
        && schema["properties"]["data"]["properties"]["receipt_pointer"]["const"]
            == surface["data"]["receipt_pointer"]
        && schema["properties"]["index_job"]["properties"]["kind"]["const"]
            == surface["index_job"]["kind"]
        && surface["index_job"]["value"].is_object()
}

fn legacy_pointer_exclusion_matches(schema: &Value, surface: &Value) -> bool {
    let exclusion = &schema["not"];
    let required = exclusion["required"].as_array();
    let data_required = exclusion["properties"]["data"]["required"].as_array();
    required.is_some_and(|required| {
        required.iter().any(|value| value == "result")
            && required.iter().any(|value| value == "data")
    }) && data_required
        .is_some_and(|required| required.iter().any(|value| value == "receipt_pointer"))
        && exclusion["properties"]["result"]["const"] == surface["result"]
        && surface["data"].get("receipt_pointer").is_some()
}

#[test]
fn keyed_index_progress_preserves_durable_owner_evidence_and_rejects_mixed_identities() {
    let key = backend_library::IndexOperationKey::from_bytes([0x51; 32]).expect("key");
    let observation = backend_library::IndexOperationObservation::Known(
        backend_library::IndexOperationStatus::new(
            key,
            PackageReference::parse(PROJECT).expect("package"),
            CompileExecutionIntent::Interactive,
            backend_library::IndexOperationState::Accepted,
        ),
    );
    let mut server = ready(Fake {
        surface_reply: Some(SurfaceReply::IndexOperationStatus(observation.clone())),
        ..Fake::default()
    });
    let result = call(
        &mut server,
        INDEX_PROGRESS_TOOL,
        &json!({"operation_key":key}),
    );
    assert_eq!(result["isError"], false);
    assert_eq!(
        result["structuredContent"]["surface"]["result"],
        "index-operation-status"
    );
    assert_eq!(
        result["structuredContent"]["surface"]["data"],
        serde_json::to_value(&observation).expect("typed evidence")
    );
    assert_eq!(
        server.product.surface_commands,
        [SurfaceCommand::IndexOperationStatus { operation_key: key }]
    );
    assert!(text_of(&result).contains("accepted"));
    assert!(!text_of(&result).contains("index operation published"));
    let next = result["structuredContent"]["index_poll"]["call"].clone();
    assert_eq!(
        next,
        json!({"name":"backend.index_progress", "arguments":{"operation_key":"51".repeat(32),"detail":"summary"}})
    );
    assert!(
        result["structuredContent"]["index_poll"]
            .get("shell")
            .is_none(),
        "no observed installed CLI in this product fixture"
    );
    assert!(text_of(&result).contains(&next.to_string()));
    server.product.surface_reply = Some(SurfaceReply::IndexOperationStatus(observation.clone()));
    let polled = call(
        &mut server,
        next["name"].as_str().expect("next tool"),
        &next["arguments"],
    );
    assert_eq!(polled["isError"], false);
    assert_eq!(
        polled["structuredContent"]["surface"]["data"],
        serde_json::to_value(&observation).expect("same exact operation")
    );
    assert_eq!(
        server.product.surface_commands,
        [
            SurfaceCommand::IndexOperationStatus { operation_key: key },
            SurfaceCommand::IndexOperationStatus { operation_key: key }
        ]
    );
    assert_context_bounded(&polled);
    for input in [
        json!({"operation_key":key,"ticket":ticket_value(&index_job_ticket())}),
        json!({"operation_key":key,"after_sequence":0}),
        json!({"operation_key":key,"compact_receipt":true}),
        json!({"operation_key":"00".repeat(32)}),
    ] {
        let mut server = ready(Fake::default());
        let result = call(&mut server, INDEX_PROGRESS_TOOL, &input);
        assert_eq!(result["isError"], true);
        assert!(server.product.surface_commands.is_empty());
    }
}

#[test]
fn index_first_use_returns_one_admitted_durable_poll_and_copying_it_only_observes() {
    use backend_library::{
        IndexOperationKey, IndexOperationObservation, IndexOperationState, IndexOperationStatus,
    };
    // A fixture for the presentation seam after the caller journal has chosen
    // its key. Closed backend decoding/admission checks the key, package and
    // intent first; this does not claim to exercise a compiler or disk journal.
    let fixture = LazyAuthorityFixture::new();
    let project = fixture.project.to_str().expect("UTF-8 project");
    let key = IndexOperationKey::from_bytes([0x52; 32]).expect("key");
    let package = PackageReference::parse(project).expect("package");
    let ticket = IndexJobTicket::new(std::num::NonZeroU64::MIN, [7; 16], package.clone());
    for state in [
        IndexOperationState::Accepted,
        IndexOperationState::Active {
            ticket,
            stage: IndexJobStage::Compiling,
        },
    ] {
        let operation = IndexOperationObservation::Known(IndexOperationStatus::new(
            key,
            package.clone(),
            CompileExecutionIntent::Interactive,
            state,
        ));
        let start = SurfaceCommand::IndexOperationStart {
            operation_key: key,
            package: package.clone(),
            execution_intent: CompileExecutionIntent::Interactive,
        };
        let reply = SurfaceReply::IndexOperationStarted(operation.clone());
        let bytes = serde_json::to_vec(&ReplyDto::new(703, CommandReply::Surface(reply.clone())))
            .expect("closed reply");
        let decoded = backend_library::decode_reply_body(&bytes).expect("closed backend wire");
        backend_library::admit_reply(
            &backend_library::CommandDto::new(
                703,
                backend_library::Command::Surface(start.clone()),
            ),
            &decoded,
        )
        .expect("exact start admission");
        for detail in ["summary", "standard", "full"] {
            let mut server = ready(Fake {
                durable_index_key: Some(key),
                surface_reply: Some(reply.clone()),
                ..Fake::default()
            });
            let wire = request(
                &mut server,
                "tools/call",
                &json!({
                    "name":"backend.index", "arguments":{"path":project, "detail":detail}
                }),
            );
            assert_eq!(wire["id"], 9);
            assert!(wire.get("error").is_none(), "{wire}");
            assert_eq!(wire["result"]["isError"], false);
            let content = &wire["result"]["structuredContent"];
            assert_eq!(content["answer"], "product");
            assert_eq!(
                content["index_operation"],
                serde_json::to_value(&operation).expect("exact durable receipt")
            );
            let next = &content["index_poll"]["call"];
            assert_eq!(next["name"], "backend.index_progress");
            assert_eq!(next["arguments"]["operation_key"], "52".repeat(32));
            assert_eq!(next["arguments"]["detail"], detail);
            let conditional = matches!(&operation, IndexOperationObservation::Known(status) if matches!(status.state, IndexOperationState::Active { .. }));
            assert_eq!(next["arguments"].get("wait_millis").is_some(), conditional);
            assert_eq!(
                next["arguments"].get("after_observation").is_some(),
                conditional
            );
            if conditional {
                assert_eq!(next["arguments"]["wait_millis"], 10000);
            }
            assert!(text_of(&wire["result"]).contains(&next.to_string()));
            assert_eq!(
                text_of(&wire["result"])
                    .matches("backend.index_progress")
                    .count(),
                1
            );
            assert_eq!(server.product.surface_commands, [start.clone()]);
            let mut line = Vec::new();
            write_message(&mut line, &wire).expect("whole production line");
            assert_stdio_line_accounting(std::str::from_utf8(&line).expect("UTF-8"));

            let mut next_operation = operation.clone();
            if conditional
                && let IndexOperationObservation::Known(status) = &mut next_operation
                && let IndexOperationState::Active { stage, .. } = &mut status.state
            {
                *stage = IndexJobStage::Publishing;
            }
            server.product.surface_reply =
                Some(SurfaceReply::IndexOperationStatus(next_operation.clone()));
            let observed = request(&mut server, "tools/call", next);
            assert_eq!(observed["id"], 9);
            assert!(observed.get("error").is_none(), "{observed}");
            assert_eq!(observed["result"]["isError"], false);
            assert_eq!(
                observed["result"]["structuredContent"]["surface"]["data"],
                serde_json::to_value(&next_operation)
                    .expect("exact next receipt; changed active stage is immediate")
            );
            let mut expected_commands = vec![
                start.clone(),
                SurfaceCommand::IndexOperationStatus { operation_key: key },
            ];
            if let IndexOperationObservation::Known(status) = &operation
                && let IndexOperationState::Active { ticket, .. } = &status.state
            {
                expected_commands.push(SurfaceCommand::IndexProgress {
                    ticket: ticket.clone(),
                    after_sequence: 0,
                });
            }
            assert_eq!(server.product.surface_commands, expected_commands);
            assert_eq!(
                server.product.probe_calls, 0,
                "copying a poll adds no other owner work"
            );
            assert_context_bounded(&observed);
        }
    }
}

#[test]
fn default_index_start_preserves_active_ticket_and_uses_durable_purl_index_api() {
    let ticket = index_job_ticket();
    let operation_key = backend_library::IndexOperationKey::from_bytes([0x72; 32]).expect("key");
    let status = backend_library::IndexOperationStatus::new(
        operation_key,
        ticket.package().clone(),
        CompileExecutionIntent::Background,
        backend_library::IndexOperationState::Active {
            ticket: ticket.clone(),
            stage: IndexJobStage::Acquiring,
        },
    );
    let reply = SurfaceReply::IndexOperationStarted(
        backend_library::IndexOperationObservation::Known(status.clone()),
    );
    let fake = Fake {
        surface_reply: Some(reply.clone()),
        durable_index_key: Some(operation_key),
        ..Fake::default()
    };
    let mut server = ready(fake);
    let result = call(
        &mut server,
        INDEX_START_TOOL,
        &json!({"package":"pkg:cargo/serde@1.0.228", "execution_intent":"background"}),
    );
    assert_eq!(result["isError"], false);
    let wire = serde_json::to_value(&reply).expect("typed reply");
    assert_eq!(
        result["structuredContent"]["surface"]["result"],
        wire["result"]
    );
    assert_eq!(result["structuredContent"]["surface"]["data"], wire["data"]);
    let poll = &result["structuredContent"]["index_poll"]["call"];
    assert_eq!(poll["name"], INDEX_PROGRESS_TOOL);
    assert_eq!(
        poll["arguments"]["operation_key"],
        serde_json::to_value(operation_key).expect("key")
    );
    assert_eq!(
        status.state,
        backend_library::IndexOperationState::Active {
            ticket,
            stage: IndexJobStage::Acquiring
        }
    );
    assert_context_bounded(&result);
    assert!(matches!(server.product.surface_commands.as_slice(),
        [SurfaceCommand::IndexOperationStart { operation_key: key, package: PackageReference::Purl(package), execution_intent: CompileExecutionIntent::Background }]
        if *key == operation_key && package.as_str() == "pkg:cargo/serde@1.0.228"
    ));
}

#[test]
fn index_progress_returns_bounded_events_and_the_exact_next_sequence() {
    let ticket = index_job_ticket();
    let profile =
        backend_library::SemanticLanguageProfile::from_name("rust").expect("closed rust profile");
    let reply = SurfaceReply::IndexProgress(IndexJobObservation::Pending(IndexProgressPage {
        ticket: ticket.clone(),
        stage: IndexJobStage::Compiling,
        events: vec![IndexJobProgressEvent {
            ticket: ticket.clone(),
            sequence: 3,
            kind: IndexJobProgressKind::ProfileStarted {
                profile,
                ordinal: 1,
                total: 2,
            },
        }]
        .into_boxed_slice(),
        next_sequence: 3,
        truncated: true,
        has_more: false,
    }));
    let mut server = ready(Fake {
        surface_reply: Some(reply.clone()),
        ..Fake::default()
    });
    let result = call(
        &mut server,
        INDEX_PROGRESS_TOOL,
        &json!({ "ticket": ticket_value(&ticket), "after_sequence": 2 }),
    );

    assert_eq!(result["isError"], false);
    let wire = serde_json::to_value(&reply).expect("typed observation");
    assert_eq!(
        result["structuredContent"]["surface"]["result"],
        wire["result"]
    );
    assert_eq!(result["structuredContent"]["surface"]["data"], wire["data"]);
    assert_eq!(
        result["structuredContent"]["surface"]["index_job"]["value"],
        wire["data"]
    );
    let text = text_of(&result);
    assert!(text.contains("older progress events aged out"), "{text}");
    assert!(text.contains("rust profile started"), "{text}");
    assert!(text.contains("\"after_sequence\":3"), "{text}");
    assert!(text.contains(&ticket_value(&ticket).to_string()), "{text}");
    assert_context_bounded(&result);
    assert!(matches!(
        server.product.surface_commands.as_slice(),
        [SurfaceCommand::IndexProgress {
            ticket: seen,
            after_sequence: 2,
        }] if seen == &ticket
    ));

    let mut server = ready(Fake {
        surface_reply: Some(reply.clone()),
        ..Fake::default()
    });
    let compact = call(
        &mut server,
        INDEX_PROGRESS_TOOL,
        &json!({
            "ticket": ticket_value(&ticket),
            "after_sequence": 2,
            "compact_receipt": true
        }),
    );
    let wire = serde_json::to_value(&reply).expect("typed observation");
    assert_compact_index_job_receipt(&compact, &wire["data"]);
    assert_eq!(
        compact["structuredContent"]["index_poll"]["call"]["arguments"]["compact_receipt"], true,
        "the recommended next ticket poll preserves the opted-in wire shape"
    );
    assert_eq!(
        compact["structuredContent"]["surface"]["index_job"]["value"]["detail"]["events"][0]["ticket"]
            ["owner_epoch"],
        serde_json::to_value([7_u8; 16]).expect("owner epoch")
    );
    assert_eq!(
        compact["structuredContent"]["surface"]["index_job"]["value"]["detail"]["events"][0]["sequence"],
        3
    );
    assert_eq!(
        server.product.surface_commands,
        [SurfaceCommand::IndexProgress {
            ticket: ticket.clone(),
            after_sequence: 2,
        }],
        "the presentation flag does not change the owner command or authority"
    );
    assert_context_bounded(&compact);

    let mut conditional_server = ready(Fake {
        surface_reply: Some(reply.clone()),
        ..Fake::default()
    });
    let conditional = call(
        &mut conditional_server,
        INDEX_PROGRESS_TOOL,
        &json!({
            "ticket": ticket_value(&ticket),
            "after_sequence": 2,
            "wait_millis": 0,
            "compact_receipt": true
        }),
    );
    assert_compact_index_job_receipt(&conditional, &wire["data"]);
    assert_context_bounded(&conditional);

    let mut unchanged_server = ready(Fake {
        surface_reply: Some(reply.clone()),
        ..Fake::default()
    });
    let unchanged = call(
        &mut unchanged_server,
        INDEX_PROGRESS_TOOL,
        &json!({
            "ticket": ticket_value(&ticket),
            "after_sequence": 3,
            "wait_millis": 0,
            "compact_receipt": true
        }),
    );
    assert_eq!(
        unchanged["structuredContent"]["surface"]["result"],
        "index-progress-unchanged"
    );
    assert_eq!(
        unchanged["structuredContent"]["index_poll"]["call"]["arguments"]["compact_receipt"],
        true
    );
    assert_context_bounded(&unchanged);
}

#[test]
fn compact_index_receipt_roundtrips_terminal_cancellation_and_partial_coverage() {
    let ticket = index_job_ticket();
    let cancelled = SurfaceReply::IndexProgress(IndexJobObservation::Terminal(IndexJobTerminal {
        ticket: ticket.clone(),
        outcome: IndexJobOutcome::Cancelled,
    }));
    let mut server = ready(Fake {
        surface_reply: Some(cancelled.clone()),
        ..Fake::default()
    });
    let response = call(
        &mut server,
        INDEX_PROGRESS_TOOL,
        &json!({"ticket":ticket_value(&ticket), "compact_receipt":true}),
    );
    let cancelled_wire = serde_json::to_value(&cancelled).expect("cancelled terminal");
    assert_compact_index_job_receipt(&response, &cancelled_wire["data"]);
    assert_eq!(
        response["structuredContent"]["surface"]["index_job"]["value"]["detail"]["outcome"]["state"],
        "cancelled"
    );
    let mut invalid = ready(Fake::default());
    let response = call(
        &mut invalid,
        INDEX_PROGRESS_TOOL,
        &json!({
            "ticket":ticket_value(&ticket),
            "compact_receipt":true,
            "detail":"full"
        }),
    );
    assert_eq!(response["isError"], true);
    assert!(invalid.product.surface_commands.is_empty());
}

fn assert_compact_index_job_receipt(response: &Value, expected: &Value) {
    let structured = &response["structuredContent"];
    let pointer = structured["surface"]["data"]["receipt_pointer"]
        .as_str()
        .expect("advertised receipt pointer");
    assert_eq!(
        structured["surface"]["data"],
        json!({"receipt_pointer":"#/surface/index_job/value"})
    );
    assert_eq!(
        structured
            .pointer(pointer.strip_prefix('#').expect("JSON pointer fragment"))
            .expect("the advertised JSON pointer resolves"),
        expected
    );
    assert_eq!(
        &structured["surface"]["index_job"]["value"],
        expected,
        "the compact response retains the entire original receipt"
    );
}

#[test]
fn index_progress_maximum_owner_page_fits_the_combined_mcp_result_budget() {
    let ticket = index_job_ticket();
    let profile =
        backend_library::SemanticLanguageProfile::from_name("rust").expect("closed rust profile");
    let events = (1..=backend_library::MAX_INDEX_PROGRESS_EVENTS as u64)
        .map(|sequence| IndexJobProgressEvent {
            ticket: ticket.clone(),
            sequence,
            kind: IndexJobProgressKind::ProfileAdmitted {
                profile,
                ordinal: 1,
                total: 1,
            },
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let reply = SurfaceReply::IndexProgress(IndexJobObservation::Pending(IndexProgressPage {
        ticket: ticket.clone(),
        stage: IndexJobStage::Publishing,
        events,
        next_sequence: backend_library::MAX_INDEX_PROGRESS_EVENTS as u64,
        truncated: true,
        has_more: true,
    }));
    let mut server = ready(Fake {
        surface_reply: Some(reply),
        ..Fake::default()
    });
    let result = call(
        &mut server,
        INDEX_PROGRESS_TOOL,
        &json!({ "ticket": ticket_value(&ticket), "after_sequence": 0 }),
    );
    assert_eq!(result["isError"], false);
    assert_context_bounded(&result);
    let text = text_of(&result);
    assert!(text.contains("more events available"), "{text}");
    assert!(text.contains("\"after_sequence\":16"), "{text}");
}

#[test]
fn index_progress_distinguishes_prior_owner_tickets_and_terminal_refusals() {
    let ticket = index_job_ticket();
    let unknown = SurfaceReply::IndexProgress(IndexJobObservation::Unknown {
        ticket: ticket.clone(),
        current_owner_epoch: [8; 16],
    });
    let mut server = ready(Fake {
        surface_reply: Some(unknown.clone()),
        ..Fake::default()
    });
    let result = call(
        &mut server,
        INDEX_PROGRESS_TOOL,
        &json!({ "ticket": ticket_value(&ticket) }),
    );
    assert_eq!(result["isError"], false);
    let wire = serde_json::to_value(&unknown).expect("unknown observation");
    assert_eq!(result["structuredContent"]["surface"]["data"], wire["data"]);
    assert_eq!(
        result["structuredContent"]["surface"]["index_job"]["value"],
        wire["data"]
    );
    assert!(text_of(&result).contains("unknown after owner restart"));

    let terminal = SurfaceReply::IndexProgress(IndexJobObservation::Terminal(IndexJobTerminal {
        ticket: ticket.clone(),
        outcome: IndexJobOutcome::Refused(
            backend_library::ProductText::new("compiler input was refused").expect("reason"),
        ),
    }));
    server.product.surface_reply = Some(terminal.clone());
    let result = call(
        &mut server,
        INDEX_PROGRESS_TOOL,
        &json!({ "ticket": ticket_value(&ticket), "after_sequence": 3 }),
    );
    assert_eq!(result["isError"], true);
    let wire = serde_json::to_value(&terminal).expect("terminal observation");
    assert_eq!(result["structuredContent"]["surface"]["data"], wire["data"]);
    assert_eq!(
        result["structuredContent"]["surface"]["index_job"]["value"],
        wire["data"]
    );
    let text = text_of(&result);
    assert!(text.contains("outcome refused"), "{text}");
    assert!(text.contains("compiler input was refused"), "{text}");
}

#[test]
fn dependency_refusal_marks_tool_failure_and_keeps_the_exact_requested_package() {
    let mut server = ready(Fake {
        surface_reply: Some(SurfaceReply::Dependencies(
            backend_library::DependencyFacts::Unavailable(
                backend_library::ProductText::from_static("package is not recorded"),
            ),
        )),
        ..Fake::default()
    });
    let result = call(
        &mut server,
        "backend.dependencies",
        &json!({"package": "pkg:npm/react@19.1.0"}),
    );
    assert_eq!(result["isError"], true);
    assert_eq!(result["structuredContent"]["answer"], "product");
    assert_eq!(
        result["structuredContent"]["fault"]["operand"],
        "pkg:npm/react@19.1.0"
    );
    assert_eq!(
        result["structuredContent"]["fault"]["call"]["arguments"]["package"],
        "pkg:npm/react@19.1.0"
    );
    assert!(text_of(&result).contains("package is not recorded"));
}

#[test]
fn copied_ticket_string_uses_the_object_decoder_and_preserves_the_owner_ticket() {
    let ticket = index_job_ticket();
    let mut server = ready(Fake::default());
    for (tool, reply, expected_command) in [
        (
            INDEX_PROGRESS_TOOL,
            SurfaceReply::IndexProgress(IndexJobObservation::Unknown {
                ticket: ticket.clone(),
                current_owner_epoch: [8; 16],
            }),
            SurfaceCommand::IndexProgress {
                ticket: ticket.clone(),
                after_sequence: 17,
            },
        ),
        (
            INDEX_CANCEL_TOOL,
            SurfaceReply::IndexCancellation(IndexCancelReceipt {
                ticket: ticket.clone(),
                status: IndexCancelStatus::Requested,
            }),
            SurfaceCommand::IndexCancel {
                ticket: ticket.clone(),
            },
        ),
    ] {
        for value in [
            ticket_value(&ticket),
            json!(ticket_value(&ticket).to_string()),
        ] {
            // Fake.surface consumes one supplied response per owner call.
            server.product.surface_reply = Some(reply.clone());
            let mut arguments = json!({"ticket": value});
            if tool == INDEX_PROGRESS_TOOL {
                arguments["after_sequence"] = json!(17);
            }
            let response = request(
                &mut server,
                "tools/call",
                &json!({"name":tool, "arguments":arguments}),
            );
            let result = &response["result"];
            assert_eq!(result["isError"], false, "{response}");
            assert_eq!(
                result["structuredContent"]["surface"]["index_job"]["value"],
                serde_json::to_value(&reply).expect("same exact reply")["data"]
            );
            assert_eq!(
                server.product.surface_commands.last(),
                Some(&expected_command)
            );
        }
        let bad = request(
            &mut server,
            "tools/call",
            &json!({"name": tool, "arguments": {"ticket": "7"}}),
        );
        assert_eq!(tool_failure(&bad, "usage")["operand"], "ticket");
    }
    assert_eq!(
        server.product.surface_commands.len(),
        4,
        "malformed tickets do not reach the owner"
    );
}

#[test]
fn missing_index_path_returns_actionable_structured_usage_before_owner_work() {
    let mut server = ready(Fake::default());
    let response = request(
        &mut server,
        "tools/call",
        &json!({"name": "backend.index", "arguments": {}}),
    );
    assert_eq!(tool_failure(&response, "usage")["operand"], "path");
    assert!(text_of(&response["result"]).contains("absolute path"));
    assert_eq!(server.product.probe_calls, 0);
}

#[test]
fn index_cancel_preserves_terminal_races_and_rejects_fabricated_tickets() {
    let ticket = index_job_ticket();
    let terminal = IndexJobTerminal {
        ticket: ticket.clone(),
        outcome: IndexJobOutcome::Published,
    };
    let reply = SurfaceReply::IndexCancellation(IndexCancelReceipt {
        ticket: ticket.clone(),
        status: IndexCancelStatus::Terminal(terminal),
    });
    let mut server = ready(Fake {
        surface_reply: Some(reply.clone()),
        ..Fake::default()
    });
    let result = call(
        &mut server,
        INDEX_CANCEL_TOOL,
        &json!({ "ticket": ticket_value(&ticket) }),
    );
    assert_eq!(result["isError"], false);
    let wire = serde_json::to_value(&reply).expect("terminal cancellation race");
    assert_eq!(result["structuredContent"]["surface"]["data"], wire["data"]);
    assert_eq!(
        result["structuredContent"]["surface"]["index_job"]["value"],
        wire["data"]
    );
    assert!(text_of(&result).contains("outcome published"));
    assert!(matches!(
        server.product.surface_commands.as_slice(),
        [SurfaceCommand::IndexCancel { ticket: seen }] if seen == &ticket
    ));

    let requested = SurfaceReply::IndexCancellation(IndexCancelReceipt {
        ticket: ticket.clone(),
        status: IndexCancelStatus::Requested,
    });
    server.product.surface_reply = Some(requested.clone());
    let result = call(
        &mut server,
        INDEX_CANCEL_TOOL,
        &json!({ "ticket": ticket_value(&ticket) }),
    );
    assert_eq!(result["isError"], false);
    let requested_wire = serde_json::to_value(&requested).expect("requested receipt");
    assert_eq!(
        result["structuredContent"]["surface"]["index_job"]["value"],
        requested_wire["data"]
    );
    let text = text_of(&result);
    assert!(text.contains("cancellation was requested"), "{text}");
    assert!(text.contains("cancellation ticket:"), "{text}");

    let malformed = request(
        &mut server,
        "tools/call",
        &json!({
            "name": INDEX_CANCEL_TOOL,
            "arguments": {
                "ticket": {
                    "id": 17,
                    "owner_epoch": [7],
                    "package": { "kind": "purl", "value": "pkg:cargo/serde@1.0.228" }
                }
            }
        }),
    );
    assert_eq!(tool_failure(&malformed, "usage")["operand"], "ticket");
    assert_eq!(server.product.surface_commands.len(), 2);
}

#[test]
fn generic_surface_progress_and_cancel_keep_the_request_ticket_projection() {
    let ticket = index_job_ticket();
    let reply = SurfaceReply::IndexCancellation(IndexCancelReceipt {
        ticket: ticket.clone(),
        status: IndexCancelStatus::Requested,
    });
    let mut server = ready(Fake {
        surface_reply: Some(reply),
        ..Fake::default()
    });
    let result = call(
        &mut server,
        SURFACE_TOOL,
        &json!({
            "command": {
                "operation": "index-cancel",
                "ticket": ticket_value(&ticket)
            }
        }),
    );
    assert_eq!(result["isError"], false);
    assert_eq!(
        result["structuredContent"]["surface"]["index_job"]["kind"],
        "cancellation"
    );
    assert_eq!(
        result["structuredContent"]["surface"]["index_job"]["value"]["ticket"],
        ticket_value(&ticket)
    );
    assert!(text_of(&result).contains(&ticket_value(&ticket).to_string()));
    assert!(matches!(
        server.product.surface_commands.as_slice(),
        [SurfaceCommand::IndexCancel { ticket: seen }] if seen == &ticket
    ));
}

#[test]
fn blocking_owner_await_is_not_exposed_over_mcp() {
    let mut server = ready(Fake::default());
    let response = request(
        &mut server,
        "tools/call",
        &json!({
            "name": "backend.index_await",
            "arguments": { "ticket": ticket_value(&index_job_ticket()) }
        }),
    );
    tool_failure(&response, "usage");
    assert!(
        text_of(&response["result"]).contains("Use backend.index_progress for bounded polling")
    );

    let command = SurfaceCommand::IndexAwait {
        ticket: index_job_ticket(),
    };
    let generic = request(
        &mut server,
        "tools/call",
        &json!({
            "name": SURFACE_TOOL,
            "arguments": { "command": serde_json::to_value(command).expect("await command") }
        }),
    );
    tool_failure(&generic, "usage");
    assert!(
        generic["result"]["structuredContent"]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("immediate bounded polling"))
    );
    assert!(server.product.surface_commands.is_empty());
}

#[test]
fn the_session_tool_list_leads_with_packages_and_index() {
    let mut server = ready(Fake::default());
    let listed = request(&mut server, "tools/list", &json!({}));
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .expect("tools is an array")
        .iter()
        .map(|tool| tool["name"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(names.first().copied(), Some("backend.packages"));
    assert_eq!(names.get(1).copied(), Some("backend.index"));
    assert!(
        names
            .iter()
            .all(|name| *name != "backend.surface" && *name != "backend.query"),
        "the session list must not advertise the escape hatches: {names:?}"
    );
}

#[test]
fn registry_lookup_tools_are_advertised_with_bounded_typed_inputs() {
    let mut server = ready(Fake::default());
    let listed = request(&mut server, "tools/list", &json!({}));
    let tools = &listed["result"]["tools"];

    let package = tool_named(tools, "backend.package");
    assert_eq!(package["inputSchema"]["required"], json!(["package"]));
    assert_eq!(
        package["inputSchema"]["properties"]["package"]["type"],
        "string"
    );
    assert_eq!(package["inputSchema"]["additionalProperties"], false);

    let search = tool_named(tools, "backend.index_search");
    assert_eq!(search["inputSchema"]["required"], json!(["query"]));
    assert_eq!(
        search["inputSchema"]["properties"]["query"]["type"],
        "string"
    );
    assert_eq!(search["inputSchema"]["properties"]["limit"]["minimum"], 1);
    assert_eq!(search["inputSchema"]["properties"]["limit"]["maximum"], 200);
    assert_eq!(
        search["inputSchema"]["properties"]["cursor"]["type"],
        "string"
    );
    assert_eq!(search["inputSchema"]["additionalProperties"], false);
}

#[test]
fn registry_schema_primitive_types_are_enforced_before_the_product_boundary() {
    let mut server = ready(Fake::default());
    let listed = request(&mut server, "tools/list", &json!({}));
    let tools = &listed["result"]["tools"];
    assert_eq!(
        tool_named(tools, "backend.search")["inputSchema"]["properties"]["limit"]["type"],
        "integer"
    );
    assert_eq!(
        tool_named(tools, "backend.read")["inputSchema"]["properties"]["coordinate"]["type"],
        "array"
    );

    for (tool, arguments, operand) in [
        (
            "backend.search",
            json!({"query":"ferris","limit":"1"}),
            "limit",
        ),
        ("backend.search", json!({"query":7}), "query"),
        (
            "backend.read",
            json!({"coordinate":DECLARATION}),
            "coordinate",
        ),
    ] {
        let response = request(
            &mut server,
            "tools/call",
            &json!({"name":tool,"arguments":arguments}),
        );
        let result = &response["result"];
        assert_eq!(result["isError"], true, "{tool} {arguments}: {response}");
        assert_eq!(result["structuredContent"]["answer"], "fault");
        assert_eq!(result["structuredContent"]["slug"], "usage");
        assert_eq!(result["structuredContent"]["operand"], operand);
        assert_context_bounded(&response);
    }
    assert_eq!(server.product.probe_calls, 0);

    let accepted = call(
        &mut server,
        "backend.search",
        &json!({"query":"ferris","limit":1}),
    );
    assert_eq!(accepted["isError"], false);
    assert_eq!(server.product.probe_calls, 1);
}

#[test]
fn missing_required_mcp_argument_names_the_tool_field_not_cli_usage() {
    let mut server = ready(Fake::default());
    for (arguments, operand, expected_detail) in [
        (json!({}), "query", "arguments.query"),
        (json!({"unknown":"x"}), "unknown", "takes no argument"),
        (
            json!({"query":7,"unknown":"x"}),
            "query",
            "must be a string",
        ),
    ] {
        let result = call(&mut server, "backend.search", &arguments);
        assert_eq!(result["isError"], true, "{arguments}");
        assert_eq!(result["structuredContent"]["answer"], "fault");
        assert_eq!(result["structuredContent"]["slug"], "usage");
        assert_eq!(result["structuredContent"]["operand"], operand);
        let message = text_of(&result);
        assert!(message.contains(expected_detail), "{message}");
        assert!(!message.contains("usage: backend search"), "{message}");
    }
    assert_eq!(server.product.probe_calls, 0);
}

#[test]
fn advertised_registry_lookups_reach_the_typed_product_commands() {
    let mut package_server = ready(Fake {
        surface_reply: Some(SurfaceReply::Package(Box::new([]))),
        ..Fake::default()
    });
    let package = call(
        &mut package_server,
        "backend.package",
        &json!({"package":"pkg:cargo/serde@1.0.228"}),
    );
    assert_eq!(package["isError"], false);
    assert!(matches!(
        package_server.product.surface_commands.as_slice(),
        [SurfaceCommand::Package { package }]
            if package.as_str() == "pkg:cargo/serde@1.0.228"
    ));

    let mut search_server = ready(Fake {
        surface_index_search_pages: true,
        ..Fake::default()
    });
    let search = call(
        &mut search_server,
        "backend.index_search",
        &json!({"query":"serde","limit":3}),
    );
    assert_eq!(search["isError"], false);
    assert!(matches!(
        search_server.product.surface_commands.as_slice(),
        [SurfaceCommand::IndexSearch { query, limit: 3, cursor: None }]
            if query.as_str() == "serde"
    ));
}

#[test]
fn index_requires_its_advertised_path_before_owner_admission() {
    let mut server = ready(Fake::default());
    let listed = request(&mut server, "tools/list", &json!({}));
    let tool = tool_named(&listed["result"]["tools"], "backend.index");
    assert_eq!(tool["inputSchema"]["required"], json!(["path"]));
    assert_eq!(tool["inputSchema"]["properties"]["path"]["type"], "string");
    assert_eq!(tool["inputSchema"]["properties"]["path"]["minLength"], 1);

    for params in [
        json!({ "name": "backend.index" }),
        json!({ "name": "backend.index", "arguments": {} }),
        json!({ "name": "backend.index", "arguments": { "detail": "full" } }),
        json!({ "name": "backend.index", "arguments": { "path": null } }),
        json!({ "name": "backend.index", "arguments": { "path": 7 } }),
        json!({ "name": "backend.index", "arguments": { "path": [] } }),
        json!({ "name": "backend.index", "arguments": { "path": "" } }),
    ] {
        let response = request(&mut server, "tools/call", &params);
        assert_eq!(response["id"], 9);
        let fault = tool_failure(&response, "usage");
        assert_eq!(fault["slug"], "usage");
        assert_eq!(fault["operand"], "path");
        assert_eq!(fault["cause"], "malformed");
        assert!(
            fault["detail"]
                .as_str()
                .expect("typed path guidance")
                .contains("backend.index requires arguments.path")
        );
        assert!(
            response["result"]["structuredContent"]["detail"]
                .as_str()
                .expect("shared path guidance")
                .contains("absolute path")
        );
        assert!(server.product.index_paths.is_empty(), "{params}");
        assert!(server.product.surface_commands.is_empty(), "{params}");
        assert_eq!(server.product.adapter_boundary, None, "{params}");
    }

    let fixture = LazyAuthorityFixture::new();
    let explicit = fixture.project.to_str().expect("UTF-8 project");
    let accepted = call(&mut server, "backend.index", &json!({ "path": explicit }));
    assert_eq!(accepted["isError"], false);
    assert_eq!(server.product.index_paths, [explicit]);
}

#[test]
fn index_tool_advertises_and_validates_execution_intent() {
    let fixture = LazyAuthorityFixture::new();
    let project = fixture.project.to_str().expect("UTF-8 project");
    let mut server = ready(Fake::default());
    let listed = request(&mut server, "tools/list", &json!({}));
    let tool = tool_named(&listed["result"]["tools"], "backend.index");
    assert_eq!(
        tool["inputSchema"]["properties"]["execution_intent"]["enum"],
        json!(["interactive", "background"])
    );
    assert_eq!(
        tool["inputSchema"]["properties"]["execution_intent"]["default"],
        "interactive"
    );
    assert!(tool["inputSchema"]["properties"]["execution-intent"].is_null());

    let accepted = call(
        &mut server,
        "backend.index",
        &json!({ "path": project, "execution_intent": "background" }),
    );
    assert_eq!(accepted["isError"], false);

    let malformed = call(
        &mut server,
        "backend.index",
        &json!({ "path": project, "execution_intent": "remote" }),
    );
    assert_eq!(malformed["isError"], true);
    assert!(
        malformed["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("choose interactive or background")
    );

    let wrong_type = call(
        &mut server,
        "backend.index",
        &json!({ "path": project, "execution_intent": ["background"] }),
    );
    assert_eq!(wrong_type["isError"], true);
    assert!(
        wrong_type["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("must be a string")
    );
}

#[test]
fn document_and_source_tools_advertise_their_useful_default_detail() {
    let mut server = ready(Fake::default());
    let listed = request(&mut server, "tools/list", &json!({}));
    for name in ["backend.document", "backend.source"] {
        let tool = tool_named(&listed["result"]["tools"], name);
        assert_eq!(
            tool["inputSchema"]["properties"]["detail"]["default"], "standard",
            "{name} must advertise the same useful default that execution applies"
        );
        assert_eq!(
            tool["inputSchema"]["properties"]["detail"]["description"],
            "Page text and fields detail level."
        );
        assert_eq!(
            tool["outputSchema"]["properties"]["answer"]["description"],
            "Tagged answer; product responses may include a fault and retain the operation receipt."
        );
        assert_eq!(
            tool["outputSchema"]["properties"]["answer"]["enum"],
            json!([
                "page", "records", "shelf", "outline", "status", "product", "fault"
            ])
        );
    }
    let search = tool_named(&listed["result"]["tools"], "backend.search");
    assert_eq!(
        search["inputSchema"]["properties"]["detail"]["default"],
        "summary"
    );
    assert_eq!(
        search["inputSchema"]["properties"]["detail"]["description"],
        "Response detail level."
    );
}

#[test]
fn explicit_source_refuses_unavailable_text_while_document_retains_its_native_identity() {
    let coordinate = format!("{PROJECT}::semantic::{}::Environment", "0b".repeat(32));
    let native = symbol_key("compiler-owned-Environment");
    for (location, excerpt, cause) in [
        (
            SourceAvailability::NotCaptured,
            SourceExcerpt::NotCaptured,
            "not-captured",
        ),
        (
            SourceAvailability::Captured(
                SourceLocation::new("httpie/context.py", 46).expect("captured site"),
            ),
            SourceExcerpt::NotHydrated,
            "not-resident",
        ),
        (
            SourceAvailability::Unconfigured,
            SourceExcerpt::Unconfigured,
            "unconfigured",
        ),
    ] {
        let document = Document::new(
            native,
            view_state_root(&[]),
            [Fragment::Text(
                "Retained compiler documentation.".to_owned(),
            )],
        )
        .with_location(location)
        .with_excerpt(excerpt);
        let mut server = ready(Fake {
            document_override: Some(document),
            ..Fake::default()
        });
        let args = json!({"coordinate":coordinate,"detail":"full"});
        let page = call(&mut server, "backend.document", &args);
        assert_eq!(page["isError"], false);
        assert_eq!(page["structuredContent"]["answer"], "page");
        assert_eq!(
            page["structuredContent"]["identity"]["coordinate"],
            coordinate
        );
        assert_eq!(
            page["structuredContent"]["identity"]["semantic_data"]["value"],
            json!(native.as_bytes())
        );
        assert_eq!(page["structuredContent"]["source_fault"]["cause"], cause);
        assert!(text_of(&page).contains("Retained compiler documentation."));

        let source = call(&mut server, "backend.source", &args);
        assert_eq!(source["isError"], true);
        assert_eq!(source["structuredContent"]["answer"], "fault");
        assert_eq!(source["structuredContent"]["slug"], "source-unavailable");
        assert_eq!(source["structuredContent"]["cause"], cause);
        assert_eq!(source["structuredContent"]["operand"], coordinate);
    }

    let mut server = ready(Fake::default());
    let captured = call(
        &mut server,
        "backend.source",
        &json!({"coordinate":DECLARATION,"detail":"full"}),
    );
    assert_eq!(captured["isError"], false);
    assert_eq!(
        captured["structuredContent"]["source"]["path"],
        "src/lib.rs"
    );
    assert_eq!(captured["structuredContent"]["source"]["line"], 2);
    assert_eq!(
        captured["structuredContent"]["source"]["lines"][0],
        "pub fn ferris() -> Beacon {"
    );
    assert!(captured["structuredContent"]["source_fault"].is_null());
}

#[test]
fn the_session_list_hides_the_surface_escape_hatch() {
    let mut server = ready(Fake::default());
    let listed = request(&mut server, "tools/list", &json!({}));
    let tools = listed["result"]["tools"].as_array().expect("tools");
    assert!(
        tools.iter().all(|tool| tool["name"] != SURFACE_TOOL),
        "backend.surface is not part of the session tool list"
    );
}

// ---------------------------------------------------------------------------
// rendering
// ---------------------------------------------------------------------------

#[test]
fn a_document_call_returns_the_markdown_page_and_the_typed_answer() {
    let mut server = ready(Fake::default());
    let result = call(
        &mut server,
        "backend.document",
        &json!({ "coordinate": DECLARATION }),
    );
    assert_eq!(result["isError"], false);
    let text = text_of(&result);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines.first().copied(),
        Some("# polyglot › src/lib.rs:2 › ferris")
    );
    assert!(
        lines
            .get(1)
            .is_some_and(|line| line.contains(" · rust · key ") && line.ends_with("src/lib.rs:2")),
        "meta line: {:?}",
        lines.get(1)
    );
    assert_eq!(
        lines.get(2).copied(),
        Some("`/abs/polyglot::src/lib.rs:2::ferris`")
    );
    assert!(text.contains("```rust\npub mod lib\n```"), "{text}");
    assert!(text.contains("Lights the beacon."), "{text}");
    assert!(text.contains("## members"), "{text}");
    assert!(text.contains("## source"), "{text}");
    assert!(text.contains("   2 pub fn ferris() -> Beacon {"), "{text}");
    assert!(!text.contains('|'), "no Markdown table may appear: {text}");
    assert_eq!(result["structuredContent"]["answer"], "page");
    assert_eq!(
        result["structuredContent"]["identity"]["coordinate"],
        DECLARATION
    );
    assert_eq!(
        result["structuredContent"]["identity"]["trail"],
        "polyglot › src/lib.rs:2 › ferris"
    );
    assert_eq!(
        result["structuredContent"]["signature"], "pub mod lib",
        "a document's default projection must include the code it was requested to show"
    );
    assert_eq!(result["structuredContent"]["source"]["extent"], "complete");
    assert!(
        result["structuredContent"]["source"]["lines"]
            .as_array()
            .is_some_and(|lines| lines
                .iter()
                .any(|line| line == "pub fn ferris() -> Beacon {")),
        "the default document projection lost its bounded source excerpt: {result}"
    );
}

#[test]
fn routed_batch_reads_keep_wire_extent_exact_text_and_signature_fallback_within_whole_line_budget()
{
    use backend_library::{DeclarationReadExtent, DeclarationRecord};
    let coordinates = [
        format!("{PROJECT}::src/weight.ts:1::WeightBurndownComponent"),
        format!("{PROJECT}::src/week.ts:1::WeekService"),
        format!("{PROJECT}::src/auth.ts:1::AuthService"),
        format!("{PROJECT}::src/missing.ts:1::MissingService"),
    ];
    let original = format!(
        "class WeightBurndownComponent {{ /* {} */ }}",
        "λ".repeat(4096)
    );
    let excerpt = SourceExcerpt::capture_bounded(&original);
    let retained = excerpt.text().expect("captured bounded prefix");
    assert_eq!(excerpt.extent(), Some(SourceExcerptExtent::Truncated));
    assert!(original.starts_with(retained));
    assert!(retained.len() <= SourceExcerpt::MAX_BYTES);
    assert!(retained.len() < original.len());
    let surface = SurfaceReply::Read(Box::new([
        DeclarationRecord {
            label: ProductText::new(&coordinates[0]).expect("requested coordinate"),
            stable_id: Some([0x11; 32]),
            signature: Some(ProductText::new(retained).expect("retained source")),
            extent: Some(DeclarationReadExtent::Truncated),
        },
        DeclarationRecord {
            label: ProductText::new(&coordinates[1]).expect("requested coordinate"),
            stable_id: Some([0x22; 32]),
            signature: Some(ProductText::new("class WeekService {}").expect("complete source")),
            extent: Some(DeclarationReadExtent::Complete),
        },
        DeclarationRecord {
            label: ProductText::new(&coordinates[2]).expect("requested coordinate"),
            stable_id: Some([0x33; 32]),
            signature: Some(ProductText::new("class AuthService").expect("signature fallback")),
            extent: None,
        },
        DeclarationRecord {
            label: ProductText::new(&coordinates[3]).expect("requested coordinate"),
            stable_id: None,
            signature: None,
            extent: None,
        },
    ]));
    surface
        .admit(backend_library::CommandId::Read)
        .expect("current read shape");
    let wire = serde_json::to_vec(&ReplyDto::new(41, CommandReply::Surface(surface.clone())))
        .expect("owner wire read");
    let decoded = backend_library::decode_reply_body(&wire).expect("strict current wire admission");
    let CommandReply::Surface(decoded) = decoded.reply else {
        panic!("read reply")
    };
    assert_eq!(
        decoded, surface,
        "wire must retain source completeness and fallback distinctions"
    );
    for detail in ["summary", "standard", "full"] {
        let mut server = ready(Fake {
            surface_reply: Some(decoded.clone()),
            ..Fake::default()
        });
        let reply = request(
            &mut server,
            "tools/call",
            &json!({
                "name":"backend.read", "arguments":{"coordinate":coordinates, "detail":detail}
            }),
        );
        let mut encoded = Vec::new();
        write_message(&mut encoded, &reply).expect("complete routed read line");
        let line = String::from_utf8(encoded).expect("UTF-8 source remains valid");
        let emitted = assert_stdio_line_accounting(&line);
        assert_eq!(emitted["id"], 9, "exact caller request id");
        assert!(emitted.get("error").is_none());
        assert_eq!(emitted["result"]["isError"], false);
        let result = &emitted["result"];
        let records = result["structuredContent"]["records"]
            .as_array()
            .expect("batch records");
        assert_eq!(records.len(), coordinates.len());
        for (record, coordinate) in records.iter().zip(&coordinates) {
            assert_eq!(record["operand"], *coordinate, "exact caller correlation");
        }
        assert_eq!(records[0]["extent"], "truncated");
        assert_eq!(records[0]["title"], retained);
        assert_eq!(records[1]["extent"], "complete");
        assert_eq!(records[1]["title"], "class WeekService {}");
        assert_eq!(records[2]["title"], "class AuthService");
        assert!(
            records[2].get("extent").is_none(),
            "a signature never claims a complete body"
        );
        assert!(
            records[3].get("extent").is_none(),
            "a missing row never claims source"
        );
        assert!(
            records[3]["tags"]
                .as_array()
                .expect("missing row tags")
                .contains(&json!("not found"))
        );
        let text = text_of(result);
        assert!(text.contains("extent: truncated"));
        assert!(text.contains("extent: complete"));
        assert!(text.contains(retained));
        assert!(matches!(server.product.surface_commands.as_slice(),
            [SurfaceCommand::Read { locators }] if locators.iter().map(ProductText::as_str)
                .eq(coordinates.iter().map(String::as_str))));
        assert_eq!(
            server.product.probe_calls, 0,
            "batch projection issues no extra owner read"
        );
    }
}

#[test]
fn page_projection_mcp_summary_omits_captured_body_and_preserves_exact_identity() {
    // The adapter fixture controls an admitted display reply. This is a
    // response-projection measurement, not native compiler/index evidence.
    let body = format!(
        "MCP_SOURCE_SENTINEL_λ\n{}",
        "retained_source_only\n".repeat(240)
    );
    assert!(body.len() > SourceExcerpt::MAX_BYTES);
    // Exercise the real producer-side prefix bound, rather than assigning a
    // truncation marker to a shorter body. This still is not a native journey.
    let excerpt = SourceExcerpt::capture_bounded(&body);
    assert_eq!(excerpt.text(), Some(&body[..SourceExcerpt::MAX_BYTES]));
    assert_eq!(excerpt.extent(), Some(SourceExcerptExtent::Truncated));
    let mut document = document();
    document.symbol = symbol_key("compiler-native-page-selection");
    document.signature = Some("fn MCP_SIGNATURE_SENTINEL() -> Beacon".to_owned());
    document = document.with_excerpt(excerpt);
    let native = document.symbol;
    let mut server = ready(Fake {
        document_override: Some(document),
        ..Fake::default()
    });
    for tool in ["backend.document", "backend.source"] {
        let response = request(
            &mut server,
            "tools/call",
            &json!({
                "name": tool, "arguments": {"coordinate": DECLARATION, "detail": "summary"}
            }),
        );
        assert_context_bounded(&response);
        let summary_bytes = serde_json::to_vec(&response)
            .expect("complete summary response")
            .len();
        let summary = &response["result"];
        assert_eq!(summary["isError"], false);
        assert_eq!(
            summary["structuredContent"]["identity"]["coordinate"],
            DECLARATION
        );
        assert_eq!(
            summary["structuredContent"]["identity"]["semantic_data"]["value"],
            json!(native.as_bytes())
        );
        assert_eq!(
            summary["structuredContent"]["signature"],
            "fn MCP_SIGNATURE_SENTINEL() -> Beacon"
        );
        assert!(summary["structuredContent"].get("source").is_none());
        assert!(summary["structuredContent"].get("source_fault").is_none());
        assert!(text_of(summary).contains("MCP_SIGNATURE_SENTINEL"));
        assert!(text_of(summary).contains(DECLARATION));
        for omitted in [
            "MCP_SOURCE_SENTINEL",
            "Lights the beacon.",
            "## source",
            "## members",
            "## relations",
        ] {
            assert!(
                !text_of(summary).contains(omitted),
                "summary leaked {omitted}"
            );
        }
        for level in [None, Some("standard"), Some("full")] {
            let mut arguments = json!({"coordinate": DECLARATION});
            if let Some(level) = level {
                arguments["detail"] = json!(level);
            }
            let response = request(
                &mut server,
                "tools/call",
                &json!({"name": tool, "arguments": arguments}),
            );
            assert_context_bounded(&response);
            let full_bytes = serde_json::to_vec(&response)
                .expect("complete source response")
                .len();
            assert!(
                full_bytes > summary_bytes + SourceExcerpt::MAX_BYTES,
                "combined response must remove both source copies at summary"
            );
            let full = &response["result"];
            assert_eq!(full["isError"], false);
            assert_eq!(
                full["structuredContent"]["identity"]["semantic_data"]["value"],
                json!(native.as_bytes())
            );
            assert_eq!(
                full["structuredContent"]["signature"],
                "fn MCP_SIGNATURE_SENTINEL() -> Beacon"
            );
            assert_eq!(full["structuredContent"]["source"]["extent"], "truncated");
            assert_eq!(
                full["structuredContent"]["source"]["lines"][0],
                "MCP_SOURCE_SENTINEL_λ"
            );
            assert_eq!(full["structuredContent"]["source"]["path"], "src/lib.rs");
            assert_eq!(full["structuredContent"]["source"]["line"], 2);
            let retained_lines = full["structuredContent"]["source"]["lines"]
                .as_array()
                .expect("retained source lines")
                .iter()
                .map(|line| line.as_str().expect("source text"))
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(
                retained_lines,
                body[..SourceExcerpt::MAX_BYTES].trim_end_matches('\n')
            );
            assert!(text_of(full).contains("MCP_SOURCE_SENTINEL_λ"));
            assert!(text_of(full).contains("source continues past the retained bound"));
            assert!(text_of(full).contains("Lights the beacon."));
        }
    }
}

#[test]
fn page_projection_mcp_summary_keeps_source_failure_and_source_tool_refusal() {
    for (location, excerpt, cause) in [
        (
            SourceAvailability::NotCaptured,
            SourceExcerpt::NotCaptured,
            "not-captured",
        ),
        (
            SourceAvailability::Captured(SourceLocation::new("src/lib.rs", 2).expect("site")),
            SourceExcerpt::NotHydrated,
            "not-resident",
        ),
        (
            SourceAvailability::Unconfigured,
            SourceExcerpt::Unconfigured,
            "unconfigured",
        ),
    ] {
        let native = symbol_key("compiler-native-unavailable-page");
        let mut document = Document::new(
            native,
            view_state_root(&[]),
            [Fragment::Text("OMITTED_DOCUMENTATION_SENTINEL".to_owned())],
        )
        .with_location(location)
        .with_excerpt(excerpt);
        document.signature = Some("fn unavailable_page()".to_owned());
        let mut server = ready(Fake {
            document_override: Some(document),
            ..Fake::default()
        });
        let arguments = json!({"coordinate": DECLARATION, "detail": "summary"});
        let page = call(&mut server, "backend.document", &arguments);
        assert_eq!(page["isError"], false);
        assert_eq!(
            page["structuredContent"]["signature"],
            "fn unavailable_page()"
        );
        assert_eq!(
            page["structuredContent"]["identity"]["semantic_data"]["value"],
            json!(native.as_bytes())
        );
        assert_eq!(page["structuredContent"]["source_fault"]["cause"], cause);
        assert!(page["structuredContent"].get("source").is_none());
        assert!(text_of(&page).contains("source-unavailable"));
        assert!(!text_of(&page).contains("OMITTED_DOCUMENTATION_SENTINEL"));
        let source = call(&mut server, "backend.source", &arguments);
        assert_eq!(source["isError"], true);
        assert_eq!(source["structuredContent"]["answer"], "fault");
        assert_eq!(source["structuredContent"]["slug"], "source-unavailable");
        assert_eq!(source["structuredContent"]["cause"], cause);
        assert_eq!(source["structuredContent"]["operand"], DECLARATION);
    }
}

#[test]
fn page_projection_mcp_combined_summary_retains_real_faults_and_section_notes() {
    let native = symbol_key("selected-page-with-section-fault");
    let identity = backend_present::Identity::parse_with_key(
        DECLARATION,
        backend_present::IdentityKey::Symbol(native),
    );
    let source_fault = Fault::source(
        &SourceAvailability::Unconfigured,
        backend_present::Operand::Coordinate(identity.coordinate().clone()),
    )
    .expect("actual source fault");
    let page = backend_present::Page::new(
        identity,
        Some(DeclarationKind::Function),
        backend_present::Source::Absent {
            fault: source_fault,
        },
    )
    .with_signature(backend_present::Signature::tokenize(
        "fn PAGE_FAULT_SIGNATURE()",
        backend_present::Language::Rust,
    ))
    .with_notes(vec![Fault::new(
        backend_present::FaultSlug::Protocol,
        backend_present::Operand::Path(PROJECT.to_owned()),
        backend_present::Cause::new(
            backend_present::CauseSlug::Unproven,
            "MCP_SECTION_FAULT_SENTINEL",
        ),
        backend_present::Affordance::UseCommand {
            name: "outline",
            args: vec![PROJECT.to_owned()].into_boxed_slice(),
        },
    )]);
    let answer = Answer::Page(Box::new(page));
    let mut server = ready(Fake::default());
    let result = server
        .rendered(
            answer.clone(),
            Detail::Summary,
            b"page-projection-context",
            CursorTarget::CliOption,
            McpInvocation::new(McpExposure::Direct, "backend.document", &Map::new()),
        )
        .expect("actual combined result projection");
    assert_eq!(
        result["isError"], true,
        "failed required section is not full success"
    );
    assert_eq!(
        result["structuredContent"]["identity"]["coordinate"],
        DECLARATION
    );
    assert_eq!(
        result["structuredContent"]["identity"]["semantic_data"]["value"],
        json!(native.as_bytes())
    );
    assert_eq!(
        result["structuredContent"]["signature"],
        "fn PAGE_FAULT_SIGNATURE()"
    );
    assert_eq!(
        result["structuredContent"]["source_fault"]["cause"],
        "unconfigured"
    );
    assert_eq!(
        result["structuredContent"]["source_fault"]["operand"],
        DECLARATION
    );
    assert_eq!(
        result["structuredContent"]["notes"][0]["detail"],
        "MCP_SECTION_FAULT_SENTINEL"
    );
    assert_eq!(result["structuredContent"]["notes"][0]["slug"], "protocol");
    assert_eq!(result["structuredContent"]["notes"][0]["cause"], "unproven");
    assert_eq!(result["structuredContent"]["notes"][0]["operand"], PROJECT);
    assert_eq!(
        result["structuredContent"]["notes"][0]["call"]["arguments"]["path"],
        PROJECT
    );
    let decoded: backend_present::SummaryPageDto =
        serde_json::from_value(result["structuredContent"].clone())
            .expect("Summary page retains its typed public fault grammar");
    assert_eq!(decoded.identity.coordinate, DECLARATION);
    let fault = decoded.source_fault.expect("retained source fault");
    assert_eq!(fault.cause, "unconfigured");
    assert_eq!(fault.operand, DECLARATION);
    assert_eq!(decoded.notes.len(), 1);
    assert_eq!(decoded.notes[0].detail, "MCP_SECTION_FAULT_SENTINEL");
    assert!(result["structuredContent"].get("source").is_none());
    let text = text_of(&result);
    assert!(text.contains(DECLARATION));
    assert!(text.contains("PAGE_FAULT_SIGNATURE"));
    assert!(text.contains("source-unavailable"));
    assert!(text.contains("this deployment has no source provider"));
    assert!(text.contains("MCP_SECTION_FAULT_SENTINEL"));
}

#[test]
fn overridden_document_still_requires_its_owner_and_current_member_basis() {
    for changed_basis in [false, true] {
        let native = symbol_key("overridden-document-member-proof");
        let basis = if changed_basis {
            view_state_root(&[("changed".to_owned(), "source".to_owned())])
        } else {
            view_state_root(&[])
        };
        let document = Document::new(
            native,
            basis,
            [Fragment::Text("RETAINED_PARTIAL_DOCUMENT".to_owned())],
        );
        let mut server = ready(Fake {
            document_override: Some(document),
            omit_document_override_member: !changed_basis,
            ..Fake::default()
        });
        let result = call(
            &mut server,
            "backend.document",
            &json!({ "coordinate": DECLARATION, "detail": "full" }),
        );
        assert_eq!(result["isError"], true);
        assert_eq!(result["structuredContent"]["answer"], "page");
        assert_eq!(
            result["structuredContent"]["notes"][0]["slug"],
            if changed_basis {
                "wrong-basis"
            } else {
                "protocol"
            }
        );
        if !changed_basis {
            assert!(
                result["structuredContent"]["notes"][0]["detail"]
                    .as_str()
                    .expect("cause")
                    .contains("document owner is absent")
            );
        }
        assert!(text_of(&result).contains("RETAINED_PARTIAL_DOCUMENT"));
    }
}

#[test]
fn a_standard_search_call_leads_with_honest_coverage_and_complete_selectable_rows() {
    let mut server = ready(Fake::default());
    let result = call(
        &mut server,
        "backend.search",
        &json!({ "query": "ferris", "detail": "standard" }),
    );
    let text = text_of(&result);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines.first().copied(),
        Some("~lanes exact· names· graph· semantic✗ unconfigured"),
        "{text}"
    );
    assert_eq!(lines.get(1).copied(), Some("• `ferris` · function · rust"));
    assert!(
        lines
            .get(2)
            .is_some_and(|line| *line == "  `/abs/polyglot::src/lib.rs:2::ferris`"),
        "{text}"
    );
    assert_eq!(result["structuredContent"]["answer"], "records");
    assert_eq!(
        result["structuredContent"]["readiness"], "unknown",
        "three lanes said nothing and one is unconfigured: neither is a failure"
    );
}

#[test]
fn standard_mcp_preview_retains_every_structured_coordinate_and_the_cli_projection() {
    let mut direct = Fake::default();
    let answer = backend_present::answer(
        &mut direct,
        &Request::Search {
            text: "ferris".to_owned(),
            limit: 25,
        },
    )
    .expect("fixture search answer");
    let projected = backend_present::encode_answer(
        &answer,
        Detail::Standard,
        None,
        DEFAULT_RESPONSE_BUDGET_BYTES,
    )
    .expect("complete shared CLI projection");
    let projected: Value = serde_json::from_slice(&projected.bytes).expect("CLI JSON");

    let mut server = ready(Fake::default());
    let result = call(
        &mut server,
        "backend.search",
        &json!({ "query": "ferris", "detail": "standard" }),
    );
    assert_eq!(result["structuredContent"], projected);
    let text = text_of(&result);
    let rows = result["structuredContent"]["records"]
        .as_array()
        .expect("whole page");
    assert_eq!(rows.len(), 1);
    for row in rows {
        let coordinate = row["identity"]["coordinate"]
            .as_str()
            .expect("exact coordinate");
        assert!(
            text.lines().any(|line| line == format!("  `{coordinate}`")),
            "{text}"
        );
        assert_eq!(
            row["identity"]["semantic_data"]["value"],
            json!(symbol_key(coordinate).as_bytes())
        );
    }
    assert!(text.contains("showing 1 exact coordinate(s); 0 omitted from text preview"));
    assert!(text.contains("Full page: `structuredContent.records`"));
    assert_eq!(result["structuredContent"]["more"], false);
}

#[test]
fn default_search_summary_keeps_exact_native_records_with_a_concise_coverage_preview() {
    let mut server = ready(Fake::default());
    let result = call(&mut server, "backend.search", &json!({"query":"ferris"}));
    let preview = text_of(&result);
    assert!(preview.starts_with("~lanes exact· names· graph· semantic✗ unconfigured"));
    assert!(preview.contains("1 row(s)"));
    assert!(preview.contains("`structuredContent.records`"));
    assert!(preview.contains(DECLARATION));
    let structured = &result["structuredContent"];
    assert_eq!(structured["detail"], "summary");
    assert_eq!(structured["more"], false);
    let records = structured["records"]
        .as_array()
        .expect("complete selected page");
    assert_eq!(records.len(), 1);
    let row = &records[0];
    assert_eq!(row["identity"]["coordinate"], DECLARATION);
    assert_eq!(row["identity"]["name"], "ferris");
    assert_eq!(row["identity"]["path"], "src/lib.rs");
    assert_eq!(row["identity"]["line"], 2);
    let retained: backend_present::IdentitySemanticData =
        serde_json::from_value(row["identity"]["semantic_data"].clone())
            .expect("the existing selected-operand type still accepts the full ID");
    let backend_present::IdentitySemanticData::SelectedSymbolId(id) = retained;
    assert_eq!(id.bytes(), symbol_key(DECLARATION).as_bytes());
    assert_eq!(row["kind"], "function");
    assert_eq!(row["language"], "rust");
    assert_eq!(row["state"], "ready");
    assert!(structured.get("nextCursor").is_none());
}

#[test]
fn positive_default_fake_outline_pages_retain_order_and_sequential_cursor() {
    fn coordinates(value: &Value, out: &mut Vec<String>) {
        if let Some(coordinate) = value.get("coordinate").and_then(Value::as_str) {
            out.push(coordinate.to_owned());
        }
        if let Some(children) = value.get("children").and_then(Value::as_array) {
            for child in children { coordinates(child, out); }
        }
    }
    let mut server = ready(Fake::default());
    let mut cursor = None;
    let mut retained = Vec::new();
    for start in 0..2 {
        let mut args = json!({"path":PROJECT,"limit":1,"detail":"full"});
        if let Some(token) = cursor.take() { args["cursor"] = Value::String(token); }
        let result = call(&mut server, "backend.outline", &args);
        assert_eq!(result["isError"], false, "{result}");
        let typed: backend_present::OutlineDto = serde_json::from_value(result["structuredContent"].clone())
            .expect("actual positive Fake page reaches typed presenter");
        assert_eq!(typed.page.as_ref().expect("bounded page").limit, 1);
        let before = retained.len();
        for root in result["structuredContent"]["roots"].as_array().expect("roots") {
            coordinates(root, &mut retained);
        }
        assert_eq!(retained.len() - before, 1, "exact fixed credit at page {start}");
        cursor = result["structuredContent"]["nextCursor"].as_str().map(str::to_owned);
        assert_eq!(cursor.is_some(), start == 0, "native sequential completion");
    }
    assert_eq!(retained, [MODULE.to_owned(), DECLARATION.to_owned()]);
    assert!(cursor.is_none());

    let override_symbol = symbol_key("selected-override-owner");
    let mut selected = document();
    selected.symbol = override_symbol;
    let mut fake = Fake { document_override: Some(selected), ..Fake::default() };
    let reply = fake.probe(Probe::OutlinePage { path: PROJECT, limit: 200 }).expect("positive override page");
    let CommandReply::ProjectionPage(page) = reply.reply else { panic!("outline page"); };
    let order = page.outline_order.as_ref().expect("positive override retains native order");
    order.validate(&page.snapshot, package_key(PROJECT), backend_library::QueryLimit::new(200).expect("limit"))
        .expect("all overridden members are exact native witnesses");
    assert!(order.symbols().contains(&override_symbol));
    let mut members_fake = Fake::default();
    let catalog = members_fake.fixture_catalog(PROJECT).expect("native members catalog");
    let owner = catalog.document(backend_library::DocumentQuery::selected(
        symbol_key(MODULE), catalog.revision_root(),
    )).expect("document at the exact native catalog revision");
    let reply = members_fake.members_page(&owner, 200, None).expect("positive separate members family");
    let CommandReply::ProjectionPage(members) = reply.reply else { panic!("members page"); };
    assert!(members.outline_order.is_none(), "outline metadata cannot leak into MembersPage");
}

#[test]
fn positive_default_fake_members_pages_preserve_credit_cursor_family_and_override() {
    let mut fake = Fake::default();
    let catalog = fake.fixture_catalog(PROJECT).expect("native members catalog");
    let owner = catalog.document(backend_library::DocumentQuery::selected(
        symbol_key(MODULE), catalog.revision_root(),
    )).expect("document at the exact native catalog revision");
    let mut cursor = None;
    let mut rows = Vec::new();
    for start in 0..2 {
        let reply = fake.members_page(&owner, 1, cursor).expect("native members selection");
        let CommandReply::ProjectionPage(page) = reply.reply else { panic!("members page"); };
        assert!(page.outline_order.is_none(), "MembersPage never carries OutlinePage metadata");
        assert_eq!(page.snapshot.root.rows().len(), 1, "actual member credit at page {start}");
        rows.extend(page.snapshot.root.rows().iter().map(|row| row.id));
        assert_eq!(page.terminal, page.snapshot.next.map_or(PageTerminal::Complete,
            |next| PageTerminal::More(PageContinuation::from_cursor(next))));
        cursor = match page.terminal {
            PageTerminal::More(next) => {
                assert_eq!(start, 0);
                let token = fake.encode_continuation(next).expect("encode actual issued members cursor");
                let decoded = fake.decode_continuation(&token).expect("decode same members cursor");
                assert_eq!(decoded, next);
                assert!(fake.members_page(&owner, 2, Some(decoded)).is_err(), "changed member credit is refused");
                Some(decoded)
            }
            PageTerminal::Complete => { assert_eq!(start, 1); None }
            other => panic!("unexpected member terminal: {other:?}"),
        };
    }
    assert_eq!(rows, [RowId::Symbol(symbol_key(MODULE)), RowId::Symbol(symbol_key(DECLARATION))]);
    assert!(cursor.is_none());

    let mut selected = document();
    selected.symbol = symbol_key("selected-override-member-owner");
    let mut overridden = Fake { document_override: Some(selected.clone()), ..Fake::default() };
    selected.basis = overridden.fixture_catalog(PROJECT)
        .expect("native catalog includes the overridden owner").revision_root();
    overridden.document_override = Some(selected.clone());
    let reply = overridden.members_page(&selected, 1, None).expect("selected override owner is present");
    let CommandReply::ProjectionPage(page) = reply.reply else { panic!("members page"); };
    assert!(page.outline_order.is_none());
    assert_eq!(page.snapshot.root.rows().len(), 1);
    assert_eq!(page.snapshot.root.rows()[0].id, RowId::Symbol(selected.symbol));
    assert_eq!(page.terminal, PageTerminal::Complete, "override has no fabricated children");
    overridden.omit_document_override_member = true;
    assert!(overridden.members_page(&selected, 1, None).is_err(), "the missing selected owner remains a refusal");
}

#[test]
fn an_outline_call_is_a_tree_of_names_never_bare_digests() {
    let mut server = ready(Fake::default());
    let result = call(&mut server, "backend.outline", &json!({}));
    let text = text_of(&result);
    assert!(text.contains("▦ lib.rs"), "{text}");
    assert!(text.contains("  ƒ ferris"), "{text}");
    assert!(text.contains("2 declaration(s) · complete"), "{text}");
    let digest = backend_present::KeyTag::from_key(symbol_key(DECLARATION).as_bytes()).to_string();
    assert!(
        !text.contains(&digest),
        "a resolved node must never fall back to its digest: {text}"
    );
    assert_eq!(result["structuredContent"]["answer"], "outline");
}

#[test]
fn status_is_a_rollup_a_reader_can_hold_not_a_capability_dump() {
    let mut server = ready(Fake::default());
    let result = call(&mut server, "backend.status", &json!({}));
    let text = text_of(&result);
    assert!(
        text.lines().count() <= 25,
        "status must stay readable, got {} lines:\n{text}",
        text.lines().count()
    );
    assert!(text.contains("oracles 0/"), "{text}");
    assert!(text.contains("no-manifest"), "{text}");
    assert!(text.contains("embedding unconfigured"), "{text}");
    assert!(text.contains("~lanes"), "{text}");
    assert_eq!(result["structuredContent"]["answer"], "status");
    assert!(
        result["structuredContent"]["capabilities"]["summary"]
            .as_str()
            .is_some_and(|summary| summary.contains("oracles 0/"))
    );
}

#[test]
fn the_packages_call_states_readiness_for_every_project() {
    let mut server = ready(Fake::default());
    let result = call(&mut server, "backend.packages", &json!({}));
    let text = text_of(&result);
    assert!(text.contains("polyglot"), "{text}");
    assert!(text.contains("`/abs/polyglot`"), "{text}");
    assert!(text.contains("rust 2"), "{text}");
    assert_eq!(result["structuredContent"]["answer"], "shelf");
    assert_eq!(
        result["structuredContent"]["projects"][0]["identity"]["coordinate"],
        PROJECT
    );
}

// ---------------------------------------------------------------------------
// faults
// ---------------------------------------------------------------------------

#[test]
fn an_unknown_coordinate_is_refused_with_the_operand_and_a_next_tool_call() {
    let mut server = ready(Fake::default());
    let result = call(
        &mut server,
        "backend.document",
        &json!({ "coordinate": MISSING }),
    );
    assert_eq!(result["isError"], true);
    let text = text_of(&result);
    assert!(
        text.starts_with("✗ not-found `/abs/polyglot::src/lib.rs:999::nothing`"),
        "{text}"
    );
    assert!(
        text.contains("no record is published at that identity"),
        "{text}"
    );
    assert_eq!(result["structuredContent"]["answer"], "fault");
    assert_eq!(result["structuredContent"]["slug"], "not-found");
    assert_eq!(
        result["structuredContent"]["operand"], MISSING,
        "the operand is never elided"
    );
    // The next step is the exact call an agent can paste back, not advice.
    assert_eq!(
        result["structuredContent"]["call"]["name"], "backend.search",
        "{result}"
    );
    assert_eq!(
        result["structuredContent"]["call"]["arguments"]["query"], "nothing",
        "the search is the reader's own word, not an invented one"
    );
    assert!(
        text.contains("→ `{\"name\":\"backend.search\",\"arguments\":{\"query\":\"nothing\"}}`"),
        "{text}"
    );
}

#[test]
#[cfg(unix)]
fn startup_wait_never_promotes_undispatched_budget_expiry_or_user_cancellation() {
    let fixture = LazyAuthorityFixture::new();
    let expired = RequestContext::new();
    assert_eq!(
        expired.read_deadline(std::time::Duration::ZERO),
        Err(ClientError::RemoteDeadlineExceeded)
    );
    assert_eq!(expired.cancellation_reason(), None);
    // Zero budget is a genuine runtime observation before any process exists.
    // The live-candidate branch requires the separate owned-child and installed
    // cold-start controls; this fixture does not manufacture a PID observation.
    let startup = backend_runtime::ensure_locald_cancellable(
        &fixture.paths,
        Some(std::time::Duration::ZERO),
        || expired.cancellation_reason().is_none(),
    )
    .expect_err("no startup dispatch within exhausted budget");
    let backend_runtime::RuntimeError::StartupPending(pending) = &startup else {
        panic!("actual undispatched pending observation required");
    };
    assert_eq!(pending.candidate_pid(), None);
    assert_eq!(
        map_runtime_startup_error(startup, Some(&expired)),
        ClientError::RemoteDeadlineExceeded
    );
    assert!(!fixture.paths.data().exists());
    assert!(!fixture.paths.endpoint().exists());
    for reason in [
        RequestCancellationReason::Explicit,
        RequestCancellationReason::ClientDisconnected,
        RequestCancellationReason::Shutdown,
    ] {
        let context = RequestContext::new();
        context
            .read_deadline(std::time::Duration::ZERO)
            .expect_err("original deadline");
        context.cancel(reason);
        let startup = backend_runtime::ensure_locald_cancellable(
            &fixture.paths,
            Some(std::time::Duration::ZERO),
            || true,
        )
        .expect_err("no dispatch");
        assert_eq!(
            map_runtime_startup_error(startup, Some(&context)),
            ClientError::RequestCancelled(reason)
        );
    }
    assert_eq!(
        map_runtime_startup_error(
            backend_runtime::RuntimeError::StartTimeout("/fixture/socket".into()),
            Some(&expired)
        ),
        ClientError::RemoteDeadlineExceeded
    );
    let active = RequestContext::new();
    let error = backend_runtime::RuntimeError::MissingExecutable("/fixture/backend-locald".into());
    let diagnostic = error.to_string();
    let ClientError::Startup(failure) = map_runtime_startup_error(error, Some(&active)) else {
        panic!("active startup retains typed cause");
    };
    assert_eq!(failure.detail(), diagnostic);
    assert_eq!(
        failure.cause(),
        backend_client::StartupFailureCause::MissingCompanion
    );
}

#[test]
fn typed_startup_failures_preserve_recovery_and_original_diagnostic_in_mcp() {
    use backend_runtime::RuntimeError;
    use std::io::{Error, ErrorKind};
    let cases = [
        (
            RuntimeError::MissingExecutable("/debug/deps/backend-locald".into()),
            backend_client::StartupFailureCause::MissingCompanion,
            false,
        ),
        (
            RuntimeError::InvalidPath("endpoint path is empty; ConnectionRefused; retry"),
            backend_client::StartupFailureCause::Configuration,
            false,
        ),
        (
            RuntimeError::Io(Error::new(
                ErrorKind::PermissionDenied,
                "private state parent /state refused",
            )),
            backend_client::StartupFailureCause::Io(ErrorKind::PermissionDenied),
            false,
        ),
        (
            RuntimeError::Spawn {
                executable: "/bin/backend-locald".into(),
                source: Error::from(ErrorKind::NotFound),
            },
            backend_client::StartupFailureCause::Spawn(ErrorKind::NotFound),
            false,
        ),
        (
            RuntimeError::DaemonExited {
                code: Some(70),
                diagnostic: None,
            },
            backend_client::StartupFailureCause::OwnerExited(Some(70)),
            false,
        ),
        (
            RuntimeError::StartTimeout("/socket".into()),
            backend_client::StartupFailureCause::StartTimeout,
            true,
        ),
        #[cfg(unix)]
        (
            RuntimeError::CompanionProtocolMismatch,
            backend_client::StartupFailureCause::CompanionMismatch,
            false,
        ),
    ];
    for (error, cause, retry) in cases {
        let original = error.to_string();
        let client = map_runtime_connect_error(error);
        let ClientError::Startup(observation) = &client else {
            panic!("startup cause was erased or treated as a disconnect: {client}");
        };
        assert_eq!(observation.cause(), cause);
        assert_eq!(observation.detail(), original);
        let fault = backend_present::probe_fault(&client, Probe::Graph(DECLARATION));
        let mut expected_dto = backend_present::fault_value(&fault);
        if retry {
            expected_dto["call"] = json!({
                "name": "backend.graph",
                "arguments": {"coordinate": DECLARATION, "limit": 1}
            });
            expected_dto
                .as_object_mut()
                .expect("fault object")
                .remove("shell");
        }
        let text = backend_present::text::fault(&fault, backend_present::Theme::plain());
        let markdown = backend_present::markdown::fault(&fault);
        assert!(text.contains(&original));
        assert!(markdown.contains(&original));
        let mut server = ready(Fake {
            adapter_error: Some(client),
            ..Fake::default()
        });
        let response = request(
            &mut server,
            "tools/call",
            &json!({
                "name": "backend.graph", "arguments": {"coordinate": DECLARATION, "limit": 1}
            }),
        );
        assert_eq!(response["id"], 9);
        let result = &response["result"];
        assert_eq!(result["isError"], true);
        assert_eq!(result["structuredContent"], expected_dto);
        for projection in [Detail::Summary, Detail::Standard, Detail::Full] {
            let payload = encode_serializable(
                "fault",
                projection,
                None,
                backend_present::FaultDto::new(&fault),
                DEFAULT_RESPONSE_BUDGET_BYTES,
            )
            .expect("bounded typed fault envelope");
            let envelope: Value =
                serde_json::from_slice(&payload.bytes).expect("encoded fault envelope");
            assert_eq!(envelope["detail"], result["structuredContent"]["detail"]);
            assert_eq!(envelope["responseDetail"], projection.name());
            for field in ["slug", "operand", "cause"] {
                assert_eq!(envelope[field], result["structuredContent"][field]);
            }
            if !retry {
                for field in ["shell", "call"] {
                    assert_eq!(envelope[field], result["structuredContent"][field]);
                }
            }
        }
        assert_context_bounded(&response);
        if retry {
            assert_eq!(
                result["structuredContent"]["call"]["arguments"],
                json!({"coordinate": DECLARATION, "limit": 1})
            );
            assert!(text_of(result).starts_with("Retry the exact tool call"));
            assert!(text_of(result).contains(&markdown));
            assert!(
                text_of(result).contains("Retry the exact tool call in structuredContent.call")
            );
            assert!(result["structuredContent"].get("shell").is_none());
        } else {
            assert_eq!(text_of(result), markdown);
            assert!(result["structuredContent"].get("shell").is_none());
            assert!(result["structuredContent"].get("call").is_none());
            assert!(!text.contains("→"));
            assert!(!markdown.contains("→"));
        }
    }
}

#[test]
fn an_unreachable_endpoint_is_a_fault_with_the_retry_affordance() {
    let mut server = ready(Fake {
        offline: true,
        ..Fake::default()
    });
    let result = call(&mut server, "backend.search", &json!({ "query": "ferris" }));
    assert_eq!(result["isError"], true);
    let text = text_of(&result);
    assert!(text.starts_with("✗ endpoint `ferris`"), "{text}");
    assert_eq!(result["structuredContent"]["cause"], "unreachable");
    assert!(result["structuredContent"].get("call").is_none());
}

#[test]
fn an_unsent_startup_retry_preserves_exact_mutation_arguments_as_advice() {
    let error = ClientError::from(backend_runtime::RuntimeError::StartTimeout(
        "/socket".into(),
    ));
    let fault = Fault::from_client_error(&error, backend_present::Operand::Argument("path".into()));
    let arguments = json!({
        "package": "/abs/project with spaces/β`",
        "operation_key": "ab".repeat(32),
        "execution_intent": "interactive",
        "detail": "full"
    });
    let result = RpcError::from_fault(&fault)
        .into_tool_result_for_call(
            "backend.index_start",
            arguments.as_object().expect("arguments"),
        )
        .expect("typed startup refusal");
    assert_eq!(result["isError"], true);
    assert_eq!(
        result["structuredContent"]["call"],
        json!({
            "name": "backend.index_start", "arguments": arguments
        })
    );
    assert!(result["structuredContent"].get("shell").is_none());
    assert!(text_of(&result).contains("structuredContent.call"));
    assert!(!text_of(&result).contains("/abs/project with spaces/β`"));
    // Prose that resembles a startup diagnostic cannot grant retry authority.
    let error = ClientError::Io("local-startup-pending; command_submitted:false".into());
    let fault = Fault::from_client_error(&error, backend_present::Operand::Argument("path".into()));
    let result = RpcError::from_fault(&fault)
        .into_tool_result_for_call(
            "backend.index_start",
            arguments.as_object().expect("arguments"),
        )
        .expect("ordinary transport refusal");
    assert!(result["structuredContent"].get("call").is_none());
}

#[test]
fn oversized_unsent_startup_call_keeps_typed_fault_without_a_partial_call() {
    let query = "x".repeat(backend_library::MAX_GRAPH_QUERY_BYTES);
    let arguments = json!({ "query": query, "variables": {} });
    let message = json!({
        "jsonrpc": "2.0",
        "id": 9,
        "method": "tools/call",
        "params": { "name": QUERY_TOOL, "arguments": arguments },
    });
    let request = serde_json::to_vec(&message).expect("bounded graph query request");
    assert!(request.len() <= crate::MAX_MCP_REQUEST_FRAME);
    assert!(backend_library::MAX_GRAPH_QUERY_BYTES.saturating_mul(2) > MCP_RESULT_BUDGET_BYTES);

    let startup = ClientError::from(backend_runtime::RuntimeError::StartTimeout(
        "/socket".into(),
    ));
    let mut server = ready(Fake {
        graph_query_error: Some(startup),
        ..Fake::default()
    });
    let response = server
        .handle(&request)
        .map(encoded_stdio_reply)
        .expect("bounded startup refusal response");
    let result = &response["result"];
    assert_eq!(result["isError"], true);
    let structured = &result["structuredContent"];
    assert_eq!(structured["slug"], "endpoint");
    assert_eq!(structured["cause"], "unreachable");
    assert!(
        structured.get("call").is_none(),
        "no partial call may escape"
    );
    assert!(structured.get("shell").is_none());
    assert!(
        structured["detail"]
            .as_str()
            .expect("startup detail")
            .contains("/socket")
    );
    assert!(
        structured["operand"]
            .as_str()
            .expect("bounded operand")
            .contains("shortened for response budget")
    );
    let text = text_of(result);
    assert!(text.contains("command was not submitted"), "{text}");
    assert!(
        text.contains("exact tool call could not be included"),
        "{text}"
    );
    assert!(text.contains("no retry was performed"), "{text}");
    assert!(
        serde_json::to_vec(&response)
            .expect("encode bounded response")
            .len()
            <= MCP_RESULT_BUDGET_BYTES
    );
}

#[test]
fn a_malformed_operand_names_the_argument_and_the_closed_set() {
    let mut server = ready(Fake::default());
    let refused = call(
        &mut server,
        "backend.search",
        &json!({ "query": "ferris", "limit": 900 }),
    );
    assert_eq!(refused["isError"], true);
    assert_eq!(refused["structuredContent"]["slug"], "usage");
    assert_eq!(refused["structuredContent"]["operand"], "limit");
    assert!(
        text_of(&refused).contains("`900`"),
        "the refused value is quoted back"
    );

    let unknown = call(
        &mut server,
        "backend.search",
        &json!({ "query": "ferris", "depth": 3 }),
    );
    assert_eq!(unknown["isError"], true);
    assert_eq!(unknown["structuredContent"]["operand"], "depth");
}

#[test]
fn an_affordance_is_the_exact_next_tool_call_an_agent_can_paste() {
    let mut server = ready(Fake::default());
    let result = call(
        &mut server,
        "backend.related",
        &json!({ "coordinate": MISSING }),
    );
    assert_eq!(result["isError"], true);
    let fault = &result["structuredContent"];
    if let Some(call) = fault.get("call").filter(|value| !value.is_null()) {
        assert!(
            call["name"]
                .as_str()
                .is_some_and(|name| name.starts_with("backend.")),
            "an affordance names a real tool: {call}"
        );
        assert!(text_of(&result).contains("→ `{"), "{}", text_of(&result));
    }
}

#[test]
fn an_unknown_tool_is_a_protocol_error_not_a_result() {
    let mut server = ready(Fake::default());
    let response = request(
        &mut server,
        "tools/call",
        &json!({ "name": "backend.nope", "arguments": {} }),
    );
    assert_eq!(response["error"]["code"], -32602);
}

// ---------------------------------------------------------------------------
// the escape hatch and the typed lane
// ---------------------------------------------------------------------------

#[test]
fn the_references_tool_serves_occurrence_sites_with_source_spans() {
    let mut server = ready(Fake::default());
    let result = call(
        &mut server,
        "backend.references",
        &json!({ "coordinate": "pkg::semantic::callee" }),
    );
    assert_eq!(result["isError"], false);
    let rendered = text_of(&result);
    assert!(
        rendered.contains("pkg::semantic::caller"),
        "the referencing site must be named: {rendered}"
    );
    assert!(
        rendered.contains("src/main.rs [bytes40..46)"),
        "the captured source span must be present: {rendered}"
    );
    assert!(
        rendered.to_lowercase().contains("compiler"),
        "the authority class is provenance: {rendered}"
    );
}

#[test]
fn the_surface_escape_hatch_returns_the_typed_reply_and_a_readable_block() {
    let mut server = ready(Fake::default());
    let result = call(
        &mut server,
        SURFACE_TOOL,
        &json!({ "command": { "operation": "subscriptions" } }),
    );
    assert_eq!(result["isError"], false);
    assert_eq!(
        result["structuredContent"]["surface"]["result"],
        "subscriptions"
    );
    assert!(text_of(&result).starts_with("# subscriptions"), "{result}");
}

#[test]
fn a_refused_surface_operation_reports_its_typed_cause() {
    let mut server = ready(Fake::default());
    let response = request(
        &mut server,
        "tools/call",
        &json!({
            "name": SURFACE_TOOL,
            "arguments": { "command": serde_json::to_value(SurfaceCommand::Diff {
                from: backend_library::PackageReference::parse("pkg:cargo/example@1.0.0")
                    .expect("admit older package"),
                to: backend_library::PackageReference::parse("pkg:cargo/example@2.0.0")
                    .expect("admit newer package"),
            })
            .expect("encode the diff command") }
        }),
    );
    tool_failure(&response, "invalid-query");
    assert!(
        response["result"]["structuredContent"]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("package is not indexed")),
        "{response}"
    );
}

#[test]
fn the_typed_query_lane_reports_its_terminal_and_revision() {
    let mut server = ready(Fake::default());
    let result = call(
        &mut server,
        QUERY_TOOL,
        &json!({ "query": "{ Declaration { coordinate @output } }" }),
    );
    assert_eq!(result["isError"], false);
    assert!(text_of(&result).starts_with("~query 0 row(s) · complete · revision "));
    assert_eq!(result["structuredContent"]["terminal"], "complete");
}

#[test]
fn graph_query_limits_are_rejected_before_the_product_boundary() {
    let mut server = ready(Fake::default());
    let oversized_query = request(
        &mut server,
        "tools/call",
        &json!({
            "name": QUERY_TOOL,
            "arguments": {
                "query": "x".repeat(backend_library::MAX_GRAPH_QUERY_BYTES + 1)
            }
        }),
    );
    tool_failure(&oversized_query, "usage");
    assert!(
        oversized_query["result"]["structuredContent"]["detail"]
            .as_str()
            .is_some_and(|message| message.contains("query exceeds"))
    );
    assert_eq!(server.product.graph_query_calls, 0);

    let mut variables = Map::new();
    for index in 0..=backend_library::MAX_GRAPH_QUERY_FIELDS {
        variables.insert(format!("v{index}"), json!(index));
    }
    let too_many_variables = request(
        &mut server,
        "tools/call",
        &json!({
            "name": QUERY_TOOL,
            "arguments": {
                "query": "{ Declaration { coordinate @output } }",
                "variables": Value::Object(variables)
            }
        }),
    );
    tool_failure(&too_many_variables, "usage");
    assert!(
        too_many_variables["result"]["structuredContent"]["detail"]
            .as_str()
            .is_some_and(|message| message.contains("field-count bound"))
    );
    assert_eq!(server.product.graph_query_calls, 0);

    let mut nested = Value::String("x".to_owned());
    for _ in 0..=backend_library::MAX_GRAPH_VALUE_DEPTH {
        nested = Value::Array(vec![nested]);
    }
    let too_deep = request(
        &mut server,
        "tools/call",
        &json!({
            "name": QUERY_TOOL,
            "arguments": {
                "query": "{ Declaration { coordinate @output } }",
                "variables": {"nested": nested}
            }
        }),
    );
    tool_failure(&too_deep, "usage");
    assert!(
        too_deep["result"]["structuredContent"]["detail"]
            .as_str()
            .is_some_and(|message| message.contains("nesting bound"))
    );
    assert_eq!(server.product.graph_query_calls, 0);

    let too_large = request(
        &mut server,
        "tools/call",
        &json!({
            "name": QUERY_TOOL,
            "arguments": {
                "query": "{ Declaration { coordinate @output } }",
                "variables": {"text": "x".repeat(backend_library::MAX_GRAPH_VALUE_BYTES + 1)}
            }
        }),
    );
    tool_failure(&too_large, "usage");
    assert!(
        too_large["result"]["structuredContent"]["detail"]
            .as_str()
            .is_some_and(|message| message.contains("byte bound"))
    );
    assert_eq!(server.product.graph_query_calls, 0);
}

// ---------------------------------------------------------------------------
// resources
// ---------------------------------------------------------------------------

#[test]
fn resources_are_the_same_markdown_their_tools_return() {
    let mut server = ready(Fake::default());
    let listed = request(&mut server, "resources/list", &json!({}));
    let resources = listed["result"]["resources"]
        .as_array()
        .expect("resources is an array");
    assert!(
        resources
            .iter()
            .all(|resource| resource["mimeType"] == "text/markdown"),
        "{listed}"
    );
    let templates = request(&mut server, "resources/templates/list", &json!({}));
    assert!(
        templates["result"]["resourceTemplates"]
            .as_array()
            .expect("templates")
            .iter()
            .any(|template| template["uriTemplate"] == "backend://outline/{path}"),
        "outlines remain addressable without claiming they exist: {templates}"
    );
    assert_eq!(
        server.product.probe_calls, 0,
        "metadata needs no owner read"
    );
    assert!(
        resources
            .iter()
            .any(|resource| resource["uri"] == "backend://tools/catalog"),
        "the capability-dependent route catalog is always addressable"
    );

    let workspace = request(
        &mut server,
        "resources/read",
        &json!({ "uri": "backend://workspace/current" }),
    );
    let text = workspace["result"]["contents"][0]["text"]
        .as_str()
        .expect("workspace resource text");
    let status = call(&mut server, "backend.status", &json!({}));
    assert_eq!(text, text_of(&status), "one renderer, one answer");
}

fn catalog_card(server: &mut Server<Fake>, name: &str) -> Value {
    catalog_card_with_id(server, name, &json!(9))
}

fn catalog_card_with_id(server: &mut Server<Fake>, name: &str, id: &Value) -> Value {
    let mut uri = format!("{}/{name}", tools::CATALOG_RESOURCE_URI);
    let mut document = String::new();
    let mut version = None;
    let mut total = None;
    for _ in 0..1024 {
        let response = server
            .handle_value(
                json!({"jsonrpc":"2.0","id":id,"method":"resources/read","params":{"uri":uri}}),
                None,
            )
            .map(encoded_stdio_reply)
            .expect("schema resource reply");
        assert!(response.get("error").is_none(), "{response}");
        assert_eq!(response["id"], *id);
        assert_context_bounded(&response);
        assert!(
            wire_tokens(&response).expect("complete catalog envelope") <= MCP_CL100K_TOKEN_LIMIT
        );
        let result = &response["result"];
        let content = &result["contents"][0];
        assert_eq!(content["uri"], uri);
        let range = &result["_meta"]["backend/catalogRange"];
        assert_eq!(range["schema"], "backend-mcp.tool-card-range/v1");
        assert_eq!(range["name"], name);
        let selected_version = range["version"].as_str().expect("exact schema version");
        assert!(
            version
                .as_ref()
                .is_none_or(|version| version == selected_version)
        );
        version = Some(selected_version.to_owned());
        let selected_total = range["totalBytes"].as_u64().expect("schema bytes");
        assert!(total.is_none_or(|total| total == selected_total));
        total = Some(selected_total);
        assert_eq!(range["offset"], document.len());
        let text = content["text"]
            .as_str()
            .expect("exact UTF-8 document range");
        document.push_str(text);
        assert_eq!(range["end"], document.len());
        assert_eq!(
            range["remainingBytes"],
            selected_total - document.len() as u64
        );
        if range["complete"] == true {
            assert_eq!(document.len() as u64, selected_total);
            assert!(result["_meta"]["backend/nextUri"].is_null());
            assert_eq!(
                blake3::hash(document.as_bytes()).to_hex().to_string(),
                selected_version
            );
            let card: Value =
                serde_json::from_str(&document).expect("entire exact schema card JSON");
            assert_eq!(card["name"], name);
            return card;
        }
        assert!(
            !text.is_empty(),
            "nonterminal ranges make monotonic progress"
        );
        assert_eq!(
            content["mimeType"], "text/plain",
            "partial JSON is not advertised as a complete JSON document"
        );
        let next = result["_meta"]["backend/nextUri"]
            .as_str()
            .expect("exact next schema URI");
        assert_ne!(next, uri);
        uri = next.to_owned();
    }
    panic!("schema ranges did not terminate");
}

#[test]
fn tool_catalog_names_callable_routes_without_advertising_unready_surfaces() {
    let mut server = ready(Fake::default());
    let listed = request(&mut server, "tools/list", &json!({}));
    let advertised: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .expect("tool list")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert_eq!(advertised.len(), 18);
    assert!(
        advertised
            .iter()
            .all(|name| tools::tool_route(name).is_some()),
        "every advertised schema has a registered route"
    );

    let catalog = request(
        &mut server,
        "resources/read",
        &json!({"uri":"backend://tools/catalog"}),
    );
    assert!(catalog.get("error").is_none(), "{catalog}");
    assert_context_bounded(&catalog);
    assert!(wire_tokens(&catalog).expect("complete catalog index") <= MCP_CL100K_TOKEN_LIMIT);
    let text = catalog["result"]["contents"][0]["text"]
        .as_str()
        .expect("catalog text");
    for name in [
        "backend.dependencies",
        "backend.dependents",
        "backend.diff",
        "backend.package_profile",
        "backend.package_source_membership",
        "backend.package_versions",
        "backend.related",
        "backend.semantic_shapes",
        "backend.semantic_versions",
    ] {
        assert!(text.contains(&format!("`{name}`")), "catalog omits {name}");
        assert!(
            !advertised.contains(&name),
            "unready route {name} was listed"
        );
        assert!(
            tools::tool_route(name).is_some(),
            "catalog route {name} is not callable"
        );
    }
    let package_versions = catalog_card(&mut server, "backend.package_versions");
    let package_versions_schema = &package_versions["inputSchema"];
    assert_eq!(
        package_versions_schema["properties"]["package"]["type"],
        "string"
    );
    assert_eq!(package_versions_schema["required"], json!(["package"]));
    let package_graph = catalog_card(&mut server, "backend.package_graph");
    let package_graph_schema = &package_graph["inputSchema"];
    assert_eq!(package_graph_schema["properties"]["limit"]["minimum"], 1);
    assert_eq!(
        package_graph_schema["properties"]["limit"]["maximum"],
        backend_library::MAX_PACKAGE_GRAPH_PAGE_ROWS
    );
    assert_eq!(package_graph_schema["properties"]["limit"]["default"], 64);

    let mut boundary = ready(Fake::default());
    let _ = call(
        &mut boundary,
        "backend.package_graph",
        &json!({
            "package": "pkg:cargo/serde@1.0.228", "direction": "dependencies", "limit": 128
        }),
    );
    assert!(matches!(
        boundary.product.surface_commands.as_slice(),
        [SurfaceCommand::PackageGraphPage { request }]
        if request.limit == 128 && request.admit().is_ok()
    ));
    boundary.product.surface_commands.clear();
    let refused = call(
        &mut boundary,
        "backend.package_graph",
        &json!({
            "package": "pkg:cargo/serde@1.0.228", "direction": "dependencies", "limit": 129
        }),
    );
    assert_eq!(refused["isError"], true);
    assert_eq!(refused["structuredContent"]["cause"], "malformed");
    assert!(
        boundary.product.surface_commands.is_empty(),
        "invalid bound reaches no owner command"
    );
    let wrong_type = call(
        &mut boundary,
        "backend.package_graph",
        &json!({
            "package": "pkg:cargo/serde@1.0.228", "direction": "dependencies", "limit": "128"
        }),
    );
    assert_eq!(wrong_type["isError"], true);
    assert!(text_of(&wrong_type).contains("an integer from 1 through 128"));
    assert!(boundary.product.surface_commands.is_empty());
    for name in [QUERY_TOOL, SURFACE_TOOL] {
        assert!(text.contains(&format!("`{name}`")), "catalog omits {name}");
        assert!(!advertised.contains(&name));
        assert!(tools::tool_route(name).is_some());
    }
    assert!(text.contains("`backend.index_await`"));
    assert_eq!(
        tools::tool_route("backend.index_await")
            .map(|route| matches!(route, tools::ToolRoute::RefusedIndexAwait)),
        Some(true)
    );
    assert!(tools::tool_route("backend.type_graph").is_none());
    assert!(!text.contains("backend.type_graph"));
    assert!(text.contains("backend://schema/query"));
}

#[test]
fn catalog_from_scratch_exposes_every_closed_route_schema_under_the_complete_budget() {
    let mut server = ready(Fake::default());
    let catalog = request(
        &mut server,
        "resources/read",
        &json!({"uri":tools::CATALOG_RESOURCE_URI}),
    );
    assert!(catalog.get("error").is_none(), "{catalog}");
    assert_context_bounded(&catalog);
    assert!(wire_tokens(&catalog).expect("complete index tokens") <= MCP_CL100K_TOKEN_LIMIT);
    let text = catalog["result"]["contents"][0]["text"]
        .as_str()
        .expect("index");
    let entries = text
        .lines()
        .filter_map(|line| {
            let line = line.strip_prefix("- `")?;
            let (name, _) = line.split_once('`')?;
            let (_, uri) = line.rsplit_once("](")?;
            Some((name, uri.strip_suffix(')')?))
        })
        .collect::<Vec<_>>();
    let actual = entries
        .iter()
        .map(|(name, _)| *name)
        .collect::<std::collections::BTreeSet<_>>();
    let expected = backend_present::GRAMMARS
        .iter()
        .map(|grammar| grammar.tool())
        .chain([QUERY_TOOL, SURFACE_TOOL, tools::CALL_TOOL])
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        actual, expected,
        "every descriptor appears once, including specialized index handlers"
    );
    assert_eq!(
        actual.len(),
        entries.len(),
        "no duplicate route hides another"
    );
    let listed = request(&mut server, "tools/list", &json!({}));
    for (name, uri) in entries {
        assert_eq!(uri, format!("{}/{name}", tools::CATALOG_RESOURCE_URI));
        let card = catalog_card(&mut server, name);
        assert!(
            !card["description"]
                .as_str()
                .expect("useful route help")
                .is_empty()
        );
        if name == SURFACE_TOOL {
            assert_eq!(card["title"], "Direct-only expert protocol guidance");
            assert!(card.get("inputSchema").is_none());
            assert!(card.get("outputSchema").is_none());
            let line = text
                .lines()
                .find(|line| line.starts_with("- `backend.surface`"))
                .expect("expert route index entry");
            assert!(line.contains("[expert protocol guidance]"));
            assert!(!line.contains("[schema and help]"));
        } else {
            assert!(
                card["inputSchema"].is_object(),
                "complete input schema for {name}"
            );
            assert!(
                card["outputSchema"].is_object(),
                "complete output schema for {name}"
            );
        }
        if card["_meta"]["backend/catalog"]["registeredTarget"] == true {
            assert_ne!(name, SURFACE_TOOL);
            assert!(card["inputSchema"].is_object());
            assert!(card["outputSchema"].is_object());
        }
        if name == QUERY_TOOL {
            let bounds = &card["_meta"]["backend/queryAdmission"];
            assert_eq!(
                bounds["maxQueryUtf8Bytes"],
                backend_library::MAX_GRAPH_QUERY_BYTES
            );
            assert_eq!(
                bounds["maxVariables"],
                backend_library::MAX_GRAPH_QUERY_FIELDS
            );
            assert_eq!(
                bounds["maxAggregateVariableBytes"],
                backend_library::MAX_GRAPH_VALUE_BYTES
            );
            assert!(
                card["inputSchema"]["properties"]["query"]
                    .get("maxLength")
                    .is_none(),
                "UTF-8 byte limit is not a Unicode character limit"
            );
            let variables = &card["inputSchema"]["properties"]["variables"]["anyOf"][1];
            assert_eq!(
                variables["maxProperties"],
                backend_library::MAX_GRAPH_QUERY_FIELDS
            );
            assert_eq!(variables["propertyNames"]["minLength"], 1);
        }
        assert_eq!(card["_meta"]["backend/catalog"]["uri"], uri);
        assert_eq!(
            card["_meta"]["backend/catalog"]["index"],
            tools::CATALOG_RESOURCE_URI
        );
        if let Some(listed) = listed["result"]["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .find(|tool| tool["name"] == name)
        {
            for field in [
                "title",
                "description",
                "inputSchema",
                "outputSchema",
                "annotations",
            ] {
                assert_eq!(
                    card[field], listed[field],
                    "schema card must retain exact MCP handler contract for {name}.{field}"
                );
            }
        }
        if let Some(grammar) = grammar_for_tool(name)
            && !matches!(
                tools::tool_route(name),
                Some(
                    ToolRoute::IndexStart
                        | ToolRoute::IndexProgress
                        | ToolRoute::IndexCancel
                        | ToolRoute::RefusedIndexAwait
                )
            )
        {
            for argument in grammar.positional().iter().chain(grammar.options()) {
                assert!(
                    !card["inputSchema"]["properties"][argument.json_name()].is_null(),
                    "missing exact operand {name}.{}",
                    argument.json_name()
                );
            }
            assert_eq!(card["inputSchema"]["additionalProperties"], false);
        }
    }
    let templates = request(&mut server, "resources/templates/list", &json!({}));
    assert!(
        templates["result"]["resourceTemplates"]
            .as_array()
            .expect("templates")
            .iter()
            .any(|template| template["uriTemplate"]
                == "backend://tools/catalog/{name}{?offset,limit,version}"
                && template.get("mimeType").is_none())
    );
    for uri in [
        "backend://tools/catalog/backend.no_such_tool",
        "backend://tools/catalog/",
        "backend://tools/catalog/backend.status/extra",
        "backend://tools/catalog/%62ackend.status",
    ] {
        let refused = request(&mut server, "resources/read", &json!({"uri":uri}));
        assert_eq!(refused["error"]["code"], -32002);
        assert!(refused.get("result").is_none());
    }
    assert_eq!(server.product.probe_calls, 0);
    assert!(
        server.product.surface_commands.is_empty(),
        "discovery never starts or reads owner work"
    );
}

#[test]
fn catalog_ranges_bind_full_schema_bytes_and_actual_ids_without_owner_dispatch() {
    let mut server = ready(Fake::default());
    let name = "backend.semantic_shapes";
    let ordinary = catalog_card(&mut server, name);
    let large_id = json!("אב🙂\\\"".repeat(500));
    let with_id = catalog_card_with_id(&mut server, name, &large_id);
    assert_eq!(
        with_id, ordinary,
        "actual ID budgeting retains every complete schema operand"
    );
    let first_uri = format!("{}/{name}", tools::CATALOG_RESOURCE_URI);
    let first = request(&mut server, "resources/read", &json!({"uri":first_uri}));
    assert!(first.get("error").is_none(), "{first}");
    let version = first["result"]["_meta"]["backend/catalogRange"]["version"]
        .as_str()
        .expect("schema version");
    let single_byte = request(
        &mut server,
        "resources/read",
        &json!({"uri":format!("{first_uri}?offset=0&limit=1&version={version}")}),
    );
    assert!(single_byte.get("error").is_none(), "{single_byte}");
    assert_eq!(
        single_byte["result"]["_meta"]["backend/catalogRange"]["offset"],
        0
    );
    assert_eq!(
        single_byte["result"]["_meta"]["backend/catalogRange"]["end"],
        1
    );
    assert_eq!(single_byte["result"]["contents"][0]["text"], "{");
    assert_eq!(
        single_byte["result"]["contents"][0]["mimeType"], "text/plain",
        "an actual partial JSON range uses fragment-compatible MIME"
    );
    assert_eq!(
        single_byte["result"]["_meta"]["backend/catalogRange"]["complete"],
        false
    );
    let bad_version = if version.starts_with('0') {
        format!("1{}", &version[1..])
    } else {
        format!("0{}", &version[1..])
    };
    let stale = request(
        &mut server,
        "resources/read",
        &json!({"uri":format!("{first_uri}?offset=1&limit=1&version={bad_version}")}),
    );
    assert_eq!(stale["error"]["code"], -32010);
    assert!(stale.get("result").is_none());
    for query in [
        "offset=1".to_owned(),
        format!("offset=0&offset=1&version={version}"),
        format!("offset=0&limit=8193&version={version}"),
        format!("offset=0&limit=0&version={version}"),
        format!("offset=0&unknown=1&version={version}"),
        format!("offset=184467440737095516160&version={version}"),
    ] {
        let refused = request(
            &mut server,
            "resources/read",
            &json!({"uri":format!("{first_uri}?{query}")}),
        );
        assert_eq!(refused["error"]["code"], -32602);
        assert!(refused.get("result").is_none());
    }
    assert_eq!(server.product.probe_calls, 0);
    assert!(server.product.surface_commands.is_empty());
}

#[test]
fn the_query_card_carries_worked_queries_over_the_declared_schema() {
    let mut server = ready(Fake::default());
    let card = request(
        &mut server,
        "resources/read",
        &json!({ "uri": "backend://schema/query" }),
    );
    let text = card["result"]["contents"][0]["text"]
        .as_str()
        .expect("schema card text")
        .to_owned();
    let queries = resources::worked_queries(&text);
    assert!(queries.len() >= 4, "the card must work through examples");
    for query in &queries {
        assert!(
            query.contains("@output"),
            "a worked query outputs something: {query}"
        );
        assert!(
            ["Declaration", "Project", "Item", "ExternalTarget"]
                .iter()
                .any(|edge| query.contains(edge)),
            "a worked query starts at a declared edge: {query}"
        );
    }
    assert!(text.contains("`coordinate` is the exact string"), "{text}");
    let parameters = text
        .split_once("```json\n")
        .and_then(|(_, block)| block.split_once("\n```"))
        .map(|(body, _)| serde_json::from_str::<Value>(body).expect("literal query call JSON"))
        .expect("query card includes one copyable tool call");
    let reply = request(&mut server, "tools/call", &parameters);
    assert_eq!(reply["result"]["isError"], false);
    let (input, limit) = server
        .product
        .graph_query_inputs
        .last()
        .expect("typed query input");
    assert_eq!(
        input.query(),
        parameters["arguments"]["query"]
            .as_str()
            .expect("query text")
    );
    assert!(input.variables().is_empty());
    assert_eq!(*limit, 25);
    assert_eq!(reply["result"]["structuredContent"]["detail"], "summary");
    assert_context_bounded(&reply);

    let mut variables = parameters.clone();
    variables["arguments"]["variables"] = json!({
        "name": "ferris", "flags": [null, true, -1, 2.5, ["nested"]]
    });
    let reply = request(&mut server, "tools/call", &variables);
    assert_eq!(reply["result"]["isError"], false);
    let (input, _) = server
        .product
        .graph_query_inputs
        .last()
        .expect("typed variables");
    assert_eq!(input.variables().len(), 2);
    assert!(matches!(&input.variables()[0], (name, GraphValue::List(_)) if name == "flags"));
    assert!(
        matches!(&input.variables()[1], (name, GraphValue::String(value)) if name == "name" && value == "ferris")
    );
    let admitted = server.product.graph_query_inputs.len();
    for invalid in [
        json!({"name": {"nested": "object"}}),
        json!({"name": [{"nested": "object"}]}),
    ] {
        variables["arguments"]["variables"] = invalid;
        let refused = request(&mut server, "tools/call", &variables);
        assert_eq!(refused["result"]["isError"], true);
        assert_eq!(server.product.graph_query_inputs.len(), admitted);
    }
}

#[test]
fn a_missing_resource_is_refused_rather_than_guessed() {
    let mut server = ready(Fake::default());
    let response = request(
        &mut server,
        "resources/read",
        &json!({ "uri": "backend://nope/x" }),
    );
    assert_eq!(response["error"]["code"], -32002);
}

#[test]
fn outline_resource_templates_preserve_readable_and_absent_projects() {
    let uri = "backend://outline/%2Fabs%2Fpolyglot";
    let mut published = ready(Fake::default());
    let catalog = published.product.fixture_catalog(PROJECT)
        .expect("resource revision and outline share the same native catalog");
    let legacy = published.product.probe(Probe::Outline(PROJECT)).expect("legacy outline");
    let CommandReply::Outline(tree) = legacy.reply else { panic!("legacy outline reply"); };
    assert_eq!(tree.basis(), catalog.revision_root());
    let names = catalog.outline_page(package_key(PROJECT), backend_library::PageRequest::new(
        catalog.revision_root(),
        backend_library::QueryLimit::new(200).expect("bounded resource name page"),
    )).expect("actual native name-page producer");
    assert_eq!(names.snapshot.root.basis().root, catalog.revision_root());
    assert_eq!(tree.source_basis(), Some(names.snapshot.root.basis()),
        "the complete tree and name-page producer bases must match");
    published.product.page_catalog = Some(catalog);
    let readable = request(&mut published, "resources/read", &json!({"uri": uri}));
    assert!(
        readable["result"]["contents"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("ferris")),
        "{readable}"
    );

    let mut empty = ready(Fake {
        empty_outline: true,
        ..Fake::default()
    });
    let listed = request(&mut empty, "resources/list", &json!({}));
    let resources = listed["result"]["resources"].as_array().expect("list");
    assert!(
        !resources.iter().any(|resource| resource["uri"] == uri),
        "{listed}"
    );
    assert!(
        resources
            .iter()
            .any(|resource| resource["uri"] == "backend://workspace/current")
    );
    let missing = request(&mut empty, "resources/read", &json!({"uri": uri}));
    assert_eq!(missing["error"]["code"], -32002, "{missing}");
    assert_eq!(missing["error"]["data"]["kind"], "not-found", "{missing}");
    assert!(
        missing["error"]["data"]["detail"]
            .as_str()
            .is_some_and(|text| text.contains("no outline is published")),
        "{missing}"
    );
}

#[test]
fn resource_discovery_needs_no_owner_and_outline_reads_preserve_protocol_failure() {
    let mut offline = ready(Fake {
        offline: true,
        ..Fake::default()
    });
    for method in ["resources/list", "resources/templates/list"] {
        let listed = request(&mut offline, method, &json!({}));
        assert!(listed.get("error").is_none(), "{listed}");
    }
    assert_eq!(offline.product.probe_calls, 0);
    let mut server = ready(Fake {
        outline_error: Some(ClientError::Protocol("invalid outline proof".to_owned())),
        ..Fake::default()
    });
    let read = request(
        &mut server,
        "resources/read",
        &json!({
            "uri": "backend://outline/%2Fabs%2Fpolyglot"
        }),
    );
    assert_eq!(read["error"]["code"], -32603, "{read}");
    assert_eq!(read["error"]["data"]["kind"], "protocol", "{read}");

    let mut moved = ready(Fake {
        outline_error: Some(ClientError::CommandFailed(
            backend_library::CommandFailure::WrongBasis {
                expected: view_state_root(&[]).into(),
                observed: view_state_root(&[("new".to_owned(), "revision".to_owned())]).into(),
            },
        )),
        ..Fake::default()
    });
    let read = request(&mut moved, "resources/read", &json!({
        "uri": "backend://outline/%2Fabs%2Fpolyglot"
    }));
    assert_eq!(read["error"]["code"], -32603, "{read}");
    assert_eq!(read["error"]["data"]["kind"], "wrong-basis", "{read}");
    assert!(read.get("result").is_none(), "a moved owner is never resource absence");
}

#[test]
fn tree_open_routes_record_the_initialized_mcp_client() {
    let mut server = ready(Fake::default());
    let opened = call(
        &mut server,
        "backend.tree_open",
        &json!({
            "subject": "declaration", "value": DECLARATION
        }),
    );
    assert_eq!(opened["isError"], false, "{opened}");
    let [
        SurfaceCommand::TreeOpen {
            opener: TreeOpener::Mcp(client),
            ..
        },
    ] = server.product.surface_commands.as_slice()
    else {
        panic!("typed MCP opener");
    };
    assert_eq!(client.as_str(), "test");
    assert!(text_of(&opened).contains("mcp"), "{opened}");
    let mut raw_command = server.product.surface_commands[0].clone();
    let SurfaceCommand::TreeOpen { opener, .. } = &mut raw_command else {
        panic!("tree open")
    };
    *opener = TreeOpener::Cli;
    let opened = call(&mut server, SURFACE_TOOL, &json!({"command": raw_command}));
    assert_eq!(opened["isError"], false, "{opened}");
    assert!(
        server
            .product
            .surface_commands
            .iter()
            .all(|command| matches!(
                command, SurfaceCommand::TreeOpen { opener: TreeOpener::Mcp(client), .. }
                if client.as_str() == "test"
            ))
    );
}

// ---------------------------------------------------------------------------
// protocol
// ---------------------------------------------------------------------------

#[test]
fn initialize_negotiates_only_the_library_supported_protocols() {
    for (offered, expected) in [
        ("2025-06-18", "2025-06-18"),
        ("2025-03-26", "2025-03-26"),
        ("2025-11-25", MCP_PROTOCOL_VERSION),
        ("2026-07-28", MCP_PROTOCOL_VERSION),
    ] {
        let mut server = Server::with_authority(Fake::default(), PROJECT.to_owned(), [9; 32]);
        let response = request(
            &mut server,
            "initialize",
            &json!({
                "protocolVersion": offered, "capabilities": {},
                "clientInfo": {"name": "qa13", "version": "1"}
            }),
        );
        assert_eq!(
            response["result"]["protocolVersion"], expected,
            "{response}"
        );
        assert_eq!(server.product.probe_calls, 0);
    }
}

#[test]
fn initialized_notification_cannot_bypass_initialize() {
    let mut server = Server::with_authority(Fake::default(), PROJECT.to_owned(), [9; 32]);
    assert!(
        server
            .handle(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_none()
    );
    let rejected = server
        .handle(br#"{"jsonrpc":"2.0","id":2,"method":"resources/read","params":{"uri":"backend://workspace/current"}}"#)
        .expect("pre-initialize requests receive an error");
    assert_eq!(rejected["error"]["code"], -32002);
    assert!(server
        .handle(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#)
        .is_some());
    let premature = server
        .handle(br#"{"jsonrpc":"2.0","id":3,"method":"tools/list","params":{}}"#)
        .expect("requests before the acknowledgement receive an error");
    assert_eq!(premature["error"]["code"], -32002);
}

#[test]
fn cancellation_notifications_are_silent_and_do_not_poison_the_session() {
    let mut server = ready(Fake::default());
    assert!(server
        .handle(br#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":9,"reason":"client stopped waiting"}}"#)
        .is_none());
    let result = call(&mut server, "backend.search", &json!({ "query": "ferris" }));
    assert_eq!(result["isError"], false);
}

#[test]
fn deeply_nested_json_is_rejected_before_tool_admission() {
    let mut server = ready(Fake::default());
    let mut nested = Value::String("x".to_owned());
    for _ in 0..=super::codec::MAX_JSON_DEPTH {
        nested = Value::Array(vec![nested]);
    }
    let request = json!({
        "jsonrpc": "2.0",
        "id": 9,
        "method": "tools/call",
        "params": {
            "name": QUERY_TOOL,
            "arguments": {
                "query": "{ Declaration { coordinate @output } }",
                "variables": { "nested": nested }
            }
        }
    });
    let response = server
        .handle(&serde_json::to_vec(&request).expect("nested request is JSON"))
        .expect("invalid requests receive a response");
    assert_eq!(response["error"]["code"], -32600);
    assert_eq!(server.product.graph_query_calls, 0);
}

#[test]
fn the_handshake_reports_the_stable_protocol_and_its_instructions() {
    let mut server = Server::with_authority(
        Fake {
            offline: true,
            ..Fake::default()
        },
        PROJECT.to_owned(),
        [9; 32],
    );
    let initialized = server
        .handle(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#)
        .expect("initialize response");
    assert_eq!(
        initialized["result"]["protocolVersion"],
        MCP_PROTOCOL_VERSION
    );
    assert_eq!(
        initialized["result"]["capabilities"],
        json!({
            "tools": {"listChanged": false},
            "resources": {"subscribe": false, "listChanged": false},
            "prompts": {"listChanged": false}
        })
    );
    let instructions = initialized["result"]["instructions"]
        .as_str()
        .unwrap_or_default();
    assert!(instructions.contains("backend.index"), "{instructions}");
    assert!(
        instructions.contains("copy the returned index_poll.call verbatim as your next backend.index_progress tool call"),
        "{instructions}"
    );
    assert!(
        instructions.contains("Repeat only while index_poll is present"),
        "{instructions}"
    );
    assert!(
        instructions.contains("inspect the terminal receipt's outcome"),
        "{instructions}"
    );
    assert!(
        instructions
            .contains("Unknown, unresolved and expired observations do not establish publication"),
        "{instructions}"
    );
    assert!(
        instructions.contains("published receipt does not make every optional lane ready"),
        "{instructions}"
    );
    assert!(
        instructions
            .find("backend.packages")
            .expect("shelf instruction")
            < instructions
                .find("backend.index")
                .expect("explicit index instruction"),
        "existing shelf inspection precedes any suggested mutation: {instructions}"
    );
    assert_eq!(
        server.product.probe_calls, 0,
        "cold handshake never needs an owner read"
    );
    assert!(
        server.product.surface_commands.is_empty(),
        "instructions admit no mutation or status poll"
    );
    let mut line = Vec::new();
    write_message(&mut line, &initialized).expect("production initialize line");
    assert_stdio_line_accounting(std::str::from_utf8(&line).expect("UTF-8"));
    assert!(instructions.contains("absolute path"), "{instructions}");
    assert!(instructions.contains("backend.document"), "{instructions}");
    assert!(
        instructions.contains("backend.index with the absolute project path"),
        "{instructions}"
    );
    assert!(
        !instructions.contains("nudox add ."),
        "the MCP host must not assume a CLI is installed"
    );
    assert!(
        instructions.contains("before backend.search"),
        "{instructions}"
    );
    assert!(
        instructions.contains(crate::process::STDIO_SETUP_GUIDANCE),
        "help and initialize must share the installed CLI's local absolute setup: {instructions}"
    );
    assert!(instructions.contains("nudox mcp-config"), "{instructions}");
    assert!(
        instructions.contains("Claude Code local-scope"),
        "{instructions}"
    );
    assert!(instructions.contains("client inside WSL"), "{instructions}");
    assert!(!instructions.contains("--scope user"), "{instructions}");
    assert!(
        !instructions.contains("CLAUDE_PROJECT_DIR"),
        "{instructions}"
    );
    assert!(
        !instructions.contains("command -v backend-mcp"),
        "{instructions}"
    );
    assert!(instructions.contains("backend.package"), "{instructions}");
    assert!(
        instructions.contains("backend.index_search"),
        "{instructions}"
    );
    assert!(
        instructions.contains("backend://workspace/current"),
        "{instructions}"
    );
}

#[test]
fn workspace_resource_projection_counts_launch_context_before_admission() {
    for working_directory in [PROJECT, "/another/checkout", "/quoted/\"λ\\path\ncheckout"] {
        let mut server = Server::with_invocation_context(
            Fake::default(),
            PROJECT.to_owned(),
            Some(working_directory.to_owned()),
            [9; 32],
        );
        request(
            &mut server,
            "initialize",
            &json!({
                "protocolVersion": "2025-06-18", "capabilities": {},
                "clientInfo": {"name": "resource-budget", "version": "1"}
            }),
        );
        assert!(
            server
                .handle(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .is_none()
        );
        let response = request(
            &mut server,
            "resources/read",
            &json!({"uri": "backend://workspace/current"}),
        );
        assert_context_bounded(&response);
        let mut projection = response["result"].clone();
        projection
            .as_object_mut()
            .expect("resource projection")
            .remove("_meta");
        let bytes = serde_json::to_vec(&projection).expect("exact resource bytes");
        assert_eq!(projection["budget"]["bytes"], bytes.len());
        assert_eq!(
            projection["budget"]["estimatedTokens"],
            bytes.len().div_ceil(4)
        );
        let text = projection["contents"][0]["text"]
            .as_str()
            .expect("resource text");
        assert!(text.contains("## MCP workspace selection"));
        assert!(text.contains(working_directory));
    }
}

#[test]
fn an_explicit_project_is_active_even_when_the_launch_directory_differs() {
    let fixture = LazyAuthorityFixture::new();
    let project = fixture.project.to_str().expect("UTF-8 project");
    let mut server = Server::with_invocation_context(
        Fake::default(),
        project.to_owned(),
        Some("/another/checkout".to_owned()),
        [9; 32],
    );
    let initialized = server
        .handle(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#)
        .expect("initialize response");
    let instructions = initialized["result"]["instructions"]
        .as_str()
        .unwrap_or_default();
    assert!(
        instructions.contains(&format!("selected project: {project}")),
        "{instructions}"
    );
    assert!(
        instructions.contains("selected project is active"),
        "{instructions}"
    );
    assert!(instructions.contains("process working directory: /another/checkout"));
    assert!(!instructions.contains("configuration mismatch"));

    assert!(
        server
            .handle(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_none()
    );
    let status = request(
        &mut server,
        "resources/read",
        &json!({ "uri": "backend://workspace/current" }),
    );
    let text = status["result"]["contents"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(
        text.contains(&format!("selected project: {project}")),
        "{text}"
    );
    assert!(text.contains("selected project is active"), "{text}");
    assert!(!text.contains("configuration mismatch"), "{text}");
    let indexed = call(&mut server, "backend.index", &json!({"path": project}));
    assert_eq!(indexed["isError"], false, "{indexed}");
    assert_eq!(server.product.index_paths, [project]);
}

#[test]
fn newline_codec_is_bounded_and_compact() {
    let mut reader = io::BufReader::new(&b"{}\n"[..]);
    assert_eq!(read_line(&mut reader).expect("read"), Some(b"{}".to_vec()));
    let mut output = Vec::new();
    write_message(&mut output, &json!({"jsonrpc":"2.0","id":1,"result":{}})).expect("write");
    assert_eq!(output.last(), Some(&b'\n'));
    assert_eq!(output.iter().filter(|byte| **byte == b'\n').count(), 1);
}

#[test]
fn newline_codec_rejects_malformed_and_oversized_jsonrpc_frames() {
    let mut oversized = vec![b'x'; crate::MAX_MCP_REQUEST_FRAME + 1];
    oversized.push(b'\n');
    let mut reader = io::BufReader::new(oversized.as_slice());
    assert!(read_line(&mut reader).is_err());

    let unterminated = vec![b'x'; crate::MAX_MCP_REQUEST_FRAME + 1];
    let mut reader = io::BufReader::new(unterminated.as_slice());
    assert!(read_line(&mut reader).is_err());

    let mut output = Vec::new();
    let oversized_result = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": "x".repeat(crate::MAX_MCP_RESPONSE_FRAME + 1)
    });
    write_message(&mut output, &oversized_result).expect("oversized reply becomes bounded fault");
    assert!(output.len() <= DEFAULT_RESPONSE_BUDGET_BYTES);
    let reply: Value = serde_json::from_slice(&output).expect("bounded refusal");
    assert_eq!(reply["error"]["code"], -32000);
    assert_eq!(reply["id"], 1);
}

#[test]
#[cfg(feature = "token-budget")]
fn bounded_response_serialization_preserves_the_calling_request_id() {
    let id = json!("request-λ-42");
    let response = token_budget_rpc_response(id.clone(), "ok", json!({"answer":"product"}), false);
    assert_eq!(response["id"], id);
    assert_eq!(response["result"]["isError"], false);
}

#[test]
fn continuation_authority_binds_context_and_owner_payload() {
    let server = Server::with_authority(Fake::default(), PROJECT.to_owned(), [7; 32]);
    let token = server
        .sign_cursor_token(FIXTURE_INDEX_CURSOR, b"query-a")
        .expect("admitted key");
    assert_eq!(
        server
            .verify_cursor_token(&token, b"query-a")
            .expect("admitted key")
            .as_deref(),
        Some(FIXTURE_INDEX_CURSOR)
    );
    // Verification is deliberately replayable: retrying a read page is safe,
    // while the MAC still prevents moving that page to another authority
    // context. This is the property a reconnecting MCP client needs.
    assert_eq!(
        server
            .verify_cursor_token(&token, b"query-a")
            .expect("admitted key")
            .as_deref(),
        Some(FIXTURE_INDEX_CURSOR)
    );
    assert!(
        server
            .verify_cursor_token(&token, b"query-b")
            .expect("admitted key")
            .is_none()
    );
    let mut tampered = token.clone();
    let final_byte = tampered.pop().expect("MAC byte");
    tampered.push(if final_byte == '0' { '1' } else { '0' });
    assert!(
        server
            .verify_cursor_token(&tampered, b"query-a")
            .expect("admitted key")
            .is_none()
    );
    let other_workspace = Server::with_authority(Fake::default(), "/other".to_owned(), [8; 32]);
    assert!(
        other_workspace
            .verify_cursor_token(&token, b"query-a")
            .expect("admitted key")
            .is_none()
    );
    // Restarting with the persisted authority accepts the token; rotation
    // invalidates every old token immediately, including one with identical
    // workspace and query context.
    let restarted = Server::with_authority(Fake::default(), PROJECT.to_owned(), [7; 32]);
    assert_eq!(
        restarted
            .verify_cursor_token(&token, b"query-a")
            .expect("admitted key")
            .as_deref(),
        Some(FIXTURE_INDEX_CURSOR)
    );
    let rotated = Server::with_authority(Fake::default(), PROJECT.to_owned(), [9; 32]);
    assert!(
        rotated
            .verify_cursor_token(&token, b"query-a")
            .expect("admitted key")
            .is_none()
    );
    let expired = server
        .sign_cursor_token_at(
            unix_seconds().saturating_sub(1),
            FIXTURE_INDEX_CURSOR,
            b"query-a",
        )
        .expect("admitted key");
    assert!(
        server
            .verify_cursor_token(&expired, b"query-a")
            .expect("admitted key")
            .is_none()
    );
    let expires_now = server
        .sign_cursor_token_at(unix_seconds(), FIXTURE_INDEX_CURSOR, b"query-a")
        .expect("admitted key");
    assert!(
        server
            .verify_cursor_token(&expires_now, b"query-a")
            .expect("admitted key")
            .is_none()
    );
}

#[test]
fn continuation_context_binds_workspace_query_limit_and_detail() {
    let mut first = Map::new();
    first.insert("query".to_owned(), json!("ferris"));
    first.insert("limit".to_owned(), json!(25));
    let mut second = first.clone();
    second.insert("query".to_owned(), json!("beacon"));
    assert_ne!(
        continuation_context(PROJECT, "backend.search", &first, Detail::Summary),
        continuation_context(PROJECT, "backend.search", &second, Detail::Summary)
    );
    assert_ne!(
        continuation_context(PROJECT, "backend.search", &first, Detail::Summary),
        continuation_context(PROJECT, "backend.search", &first, Detail::Full)
    );
    assert_ne!(
        continuation_context(PROJECT, "backend.search", &first, Detail::Summary),
        continuation_context(PROJECT, "backend.document", &first, Detail::Summary)
    );
    assert_ne!(
        continuation_context(PROJECT, "backend.search", &first, Detail::Summary),
        continuation_context("/other", "backend.search", &first, Detail::Summary)
    );
    let mut spaced = first.clone();
    spaced.insert("query".to_owned(), json!("  ferris   "));
    assert_ne!(
        continuation_context(PROJECT, "backend.search", &first, Detail::Summary),
        continuation_context(PROJECT, "backend.search", &spaced, Detail::Summary)
    );
    let mut different_limit = first;
    different_limit.insert("limit".to_owned(), json!(26));
    assert_ne!(
        continuation_context(PROJECT, "backend.search", &spaced, Detail::Summary),
        continuation_context(PROJECT, "backend.search", &different_limit, Detail::Summary)
    );
}

#[test]
fn every_metadata_route_stays_within_the_context_budget() {
    let mut server = Server::with_authority(Fake::default(), PROJECT.to_owned(), [9; 32]);
    let initialized = server
        .handle(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#)
        .expect("initialize response");
    assert_context_bounded(&initialized);
    assert!(
        server
            .handle(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_none()
    );

    let routes = [
        ("ping", json!({})),
        ("tools/list", json!({})),
        ("resources/list", json!({})),
        ("resources/templates/list", json!({})),
        ("prompts/list", json!({})),
        (
            "prompts/get",
            json!({"name":"backend.explore","arguments":{"query":"λ אב"}}),
        ),
        (
            "resources/read",
            json!({"uri":"backend://workspace/current"}),
        ),
        ("resources/read", json!({"uri":"backend://schema/query"})),
        (
            "tools/call",
            json!({"name":"backend.packages","arguments":{}}),
        ),
        (
            "tools/call",
            json!({"name":"backend.search","arguments":{"query":"λ אב"}}),
        ),
        (
            "tools/call",
            json!({"name":"backend.document","arguments":{"coordinate":MODULE}}),
        ),
        (
            "tools/call",
            json!({"name":QUERY_TOOL,"arguments":{"query":"{ Declaration { coordinate @output } }"}}),
        ),
    ];
    for (method, params) in routes {
        let response = request(&mut server, method, &params);
        assert_context_bounded(&response);
    }
    let huge_prompt = json!({
        "jsonrpc":"2.0",
        "id": "id-".to_owned() + &"אב".repeat(DEFAULT_RESPONSE_BUDGET_BYTES),
        "method":"prompts/get",
        "params":{"name":"backend.explore","arguments":{"query":"long"}}
    });
    let response = server
        .handle(&serde_json::to_vec(&huge_prompt).expect("large prompt request"))
        .expect("large prompt response");
    assert_context_bounded(&encoded_stdio_reply(response));
}

#[test]
fn wire_budget_metadata_counts_the_complete_escaped_jsonrpc_line() {
    let source_text = "λ אב🙂\n".repeat(DEFAULT_RESPONSE_BUDGET_BYTES);
    let structured = json!({
        "answer":"records",
        "records":[{"coordinate":"/selected/current"}],
        "more":true,
        "nextCursor":"opaque-owner-cursor-page-two",
        "budget":{"bytes":34050,"estimatedTokens":8513,"bytesPerToken":4}
    });
    let result = tool_result(&source_text, structured.clone(), false);
    let response = encoded_stdio_reply(success(json!("request-אב🙂"), result));
    assert_context_bounded(&response);
    assert_eq!(response["result"]["structuredContent"], structured);
    assert_eq!(response["result"]["isError"], false);

    let preview = text_of(&response["result"]);
    assert!(preview.starts_with("λ אב🙂\n"));
    assert!(preview.len() < source_text.len());
    assert!(preview.ends_with(
        "… readable preview shortened to fit this response; structuredContent contains the complete page and any nextCursor; when present, copy structuredContent.continuation_call unchanged."
    ));

    let wire_budget = &response["result"]["_meta"]["backend/wireBudget"];
    let actual_bytes = serde_json::to_vec(&response)
        .expect("response serializes")
        .len()
        .saturating_add(1);
    assert_eq!(wire_budget["bytes"], actual_bytes);
    assert_eq!(
        wire_budget["estimatedTokens"],
        estimate_tokens(actual_bytes)
    );
    assert_eq!(wire_budget["bytesPerToken"], ESTIMATED_BYTES_PER_TOKEN);
    assert_eq!(wire_budget["limitBytes"], DEFAULT_RESPONSE_BUDGET_BYTES);
    assert_eq!(wire_budget["scope"], "complete_jsonrpc_line");

    let large_id = format!("request-{}", "אב🙂".repeat(2_000));
    let large_structured = json!({
        "answer":"records",
        // Same byte pressure, but legal under the named token bound too.
        // The original dense `s` payload remains a refusal control below.
        "rows":" ".repeat(31_000),
        "more":true,
        "nextCursor":"opaque-owner-cursor-page-two"
    });
    let large_text = "λ אב🙂\n".repeat(6_000);
    let large_result = tool_result(&large_text, large_structured.clone(), false);
    let large_response = encoded_stdio_reply(success(json!(large_id.clone()), large_result));
    assert_eq!(large_response["id"], large_id);
    assert_eq!(large_response["result"]["isError"], false);
    assert_eq!(
        large_response["result"]["structuredContent"],
        large_structured
    );
    assert!(text_of(&large_response["result"]).ends_with(
        "… readable preview shortened to fit this response; structuredContent contains the complete page and any nextCursor; when present, copy structuredContent.continuation_call unchanged."
    ));
    assert_context_bounded(&large_response);
    let large_wire_bytes = serde_json::to_vec(&large_response)
        .expect("large-id response serializes")
        .len()
        .saturating_add(1);
    assert_eq!(
        large_response["result"]["_meta"]["backend/wireBudget"]["bytes"],
        large_wire_bytes
    );
}

#[test]
fn graph_continuation_round_trip_is_bounded_and_authorized() {
    for limit in [1, 200] {
        let mut server = ready(Fake {
            graph_continue: true,
            ..Fake::default()
        });
        let first = call(
            &mut server,
            QUERY_TOOL,
            &json!({"query":"{ Declaration { coordinate @output } }","limit":limit}),
        );
        assert_context_bounded(&first);
        assert_eq!(first["isError"], false);
        assert_eq!(first["structuredContent"]["terminal"], "limit_reached");
        let cursor = first["structuredContent"]["nextCursor"]
            .as_str()
            .expect("first graph page carries a cursor")
            .to_owned();
        let preview = text_of(&first);
        assert!(preview.contains("Continue `backend.query` with the same arguments"));
        assert!(preview.contains("copy `structuredContent.nextCursor` to `arguments.cursor`"));
        assert!(!preview.contains(&format!("`{cursor}`")));
        assert!(!preview.contains("raise `limit`"), "{preview}");
        assert!(!preview.contains("narrow the query"), "{preview}");

        let second = call(
            &mut server,
            QUERY_TOOL,
            &json!({
                "query":"{ Declaration { coordinate @output } }",
                "limit":limit,
                "cursor":cursor
            }),
        );
        assert_context_bounded(&second);
        assert_eq!(second["isError"], false);
        assert_eq!(second["structuredContent"]["terminal"], "complete");
    }
}

#[test]
fn graph_page_text_does_not_treat_cancellation_or_missing_cursor_as_limit_advice() {
    let cancelled = GraphQueryPage {
        revision: view_state_root(&[]).into(),
        source: basis().object,
        rows: Box::new([]),
        terminal: PageTerminal::Cancelled,
    };
    let cancelled_text = super::graph_page_text(&cancelled, None);
    assert!(cancelled_text.contains("query cancelled before completion"));
    assert!(!cancelled_text.contains("raise `limit`"));
    assert!(!cancelled_text.contains("more rows"));

    let more_without_token = GraphQueryPage {
        terminal: PageTerminal::More(PageContinuation::from_cursor(backend_library::Cursor::new())),
        ..cancelled
    };
    let unavailable_text = super::graph_page_text(&more_without_token, None);
    assert!(unavailable_text.contains("no continuation cursor was issued"));
    assert!(!unavailable_text.contains("raise `limit`"));
}

#[test]
fn surface_index_search_cursor_round_trips_through_the_mcp_projection() {
    let mut first_server = ready(Fake {
        surface_index_search_pages: true,
        ..Fake::default()
    });
    let first = call(
        &mut first_server,
        SURFACE_TOOL,
        &json!({
            "command": {
                "operation": "index-search",
                "query": "maven",
                "limit": 1
            },
            "detail": "full"
        }),
    );
    assert_eq!(first["isError"], false);
    assert_context_bounded(&first);
    let cursor = first["structuredContent"]["surface"]["data"]["next_cursor"]
        .as_str()
        .expect("index-search page carries a nested cursor")
        .to_owned();
    assert!(cursor.starts_with("mcp2-"));
    assert!(cursor.contains(FIXTURE_INDEX_CURSOR));
    assert!(!text_of(&first).contains(&cursor));
    assert!(text_of(&first).contains("`command.cursor`"));
    assert!(!text_of(&first).contains("`--cursor`"));
    assert!(
        serde_json::to_string(&first)
            .expect("tool result JSON")
            .contains(FIXTURE_INDEX_CURSOR)
    );

    let structured = &first["structuredContent"];
    assert_eq!(structured["surface"]["result"], "index-search-page");
    let measured = structured["budget"]["bytes"]
        .as_u64()
        .expect("typed surface carries measured bytes");
    let measured = usize::try_from(measured).expect("measured bytes fit this host");
    assert_eq!(
        serde_json::to_vec(structured)
            .expect("structured result JSON")
            .len(),
        measured,
        "the signed cursor is inside the budgeted typed projection"
    );
    let final_reply_bytes = serde_json::to_vec(&first)
        .expect("complete MCP response including Markdown")
        .len();
    assert!(
        final_reply_bytes <= DEFAULT_RESPONSE_BUDGET_BYTES,
        "structured surface and signed-token Markdown fit together: {final_reply_bytes}"
    );

    // The server-side MAC is portable across an MCP process restart when its
    // authority secret and project selection are unchanged.
    let mut restarted = ready(Fake {
        surface_index_search_pages: true,
        ..Fake::default()
    });
    let second = call(
        &mut restarted,
        SURFACE_TOOL,
        &json!({
            "command": {
                "operation": "index-search",
                "query": "maven",
                "limit": 1,
                "cursor": cursor
            },
            "detail": "full"
        }),
    );
    assert_eq!(second["isError"], false);
    assert_eq!(
        restarted.product.surface_index_search_seen,
        vec![Some(FIXTURE_INDEX_CURSOR.to_owned())]
    );
    assert!(second["structuredContent"]["surface"]["data"]["next_cursor"].is_null());
    assert!(!text_of(&second).contains("next cursor"));
}

#[test]
fn index_search_tool_round_trips_mcp_cursors_at_summary_and_full_detail() {
    for detail in ["summary", "full"] {
        let mut server = ready(Fake {
            surface_index_search_pages: true,
            ..Fake::default()
        });
        let first = call(
            &mut server,
            "backend.index_search",
            &json!({"query":"maven","limit":1,"detail":detail}),
        );
        assert_eq!(first["isError"], false, "detail={detail}");
        assert_context_bounded(&first);
        let cursor = first["structuredContent"]["index_search_page"]["next_cursor"]
            .as_str()
            .expect("product page carries the nested cursor")
            .to_owned();
        assert!(cursor.starts_with("mcp2-"));
        assert_eq!(
            first["structuredContent"]["nextCursor"].as_str(),
            Some(cursor.as_str())
        );
        assert!(!text_of(&first).contains(&cursor));
        assert!(text_of(&first).contains("`backend.index_search`"));
        assert!(
            serde_json::to_string(&first)
                .expect("tool result JSON")
                .contains(FIXTURE_INDEX_CURSOR)
        );
        let structured = &first["structuredContent"];
        let measured = usize::try_from(
            structured["budget"]["bytes"]
                .as_u64()
                .expect("typed product carries measured bytes"),
        )
        .expect("measured bytes fit this host");
        assert_eq!(
            serde_json::to_vec(structured)
                .expect("structured product JSON")
                .len(),
            measured,
            "both signed cursor projections are inside the measured budget"
        );

        let second = call(
            &mut server,
            "backend.index_search",
            &json!({
                "query":"maven",
                "limit":1,
                "detail":detail,
                "cursor":cursor
            }),
        );
        assert_eq!(second["isError"], false, "detail={detail}");
        assert_eq!(
            server.product.surface_index_search_seen,
            vec![None, Some(FIXTURE_INDEX_CURSOR.to_owned())]
        );
        assert!(
            second["structuredContent"]["index_search_page"]
                .get("next_cursor")
                .is_none()
        );
        assert!(second["structuredContent"].get("nextCursor").is_none());
        assert!(!text_of(&second).contains("next cursor"));

        // The MCP authority signature survives a process reconstruction when
        // the persisted secret and selected project are unchanged.
        let mut restarted = ready(Fake {
            surface_index_search_pages: true,
            ..Fake::default()
        });
        let after_restart = call(
            &mut restarted,
            "backend.index_search",
            &json!({
                "query":"maven",
                "limit":1,
                "detail":detail,
                "cursor":cursor
            }),
        );
        assert_eq!(after_restart["isError"], false, "detail={detail}");
        assert_eq!(
            restarted.product.surface_index_search_seen,
            vec![Some(FIXTURE_INDEX_CURSOR.to_owned())]
        );
        assert!(
            after_restart["structuredContent"]
                .get("nextCursor")
                .is_none()
        );
    }
}

#[test]
fn index_search_cursor_round_trips_between_servers_with_the_same_workspace_authority() {
    let mut first_server = ready(Fake {
        surface_index_search_pages: true,
        ..Fake::default()
    });
    let first = call(
        &mut first_server,
        "backend.index_search",
        &json!({"query":"maven","limit":1,"detail":"summary"}),
    );
    let cursor = first["structuredContent"]["index_search_page"]["next_cursor"]
        .as_str()
        .expect("first page carries a signed cursor")
        .to_owned();

    // MCP server processes reconstructed from the same workspace authority
    // and exact project/query context can resume durable owner cursors.
    let mut same_authority = ready(Fake {
        surface_index_search_pages: true,
        ..Fake::default()
    });
    let resumed = call(
        &mut same_authority,
        "backend.index_search",
        &json!({
            "query":"maven",
            "limit":1,
            "detail":"summary",
            "cursor":cursor
        }),
    );
    assert_eq!(resumed["isError"], false);
    assert_eq!(
        same_authority.product.surface_index_search_seen,
        vec![Some(FIXTURE_INDEX_CURSOR.to_owned())]
    );

    // The same token cannot be replayed under a different workspace key or
    // project selection, and neither rejected request reaches the owner.
    for mismatch in ["authority", "project"] {
        let mut other = ready(Fake {
            surface_index_search_pages: true,
            ..Fake::default()
        });
        match mismatch {
            "authority" => other.cursor_secret = [8; 32].into(),
            "project" => other.project = "/other-project".to_owned(),
            _ => unreachable!(),
        }
        let response = request(
            &mut other,
            "tools/call",
            &json!({
                "name":"backend.index_search",
                "arguments":{
                    "query":"maven",
                    "limit":1,
                    "detail":"summary",
                    "cursor":cursor
                }
            }),
        );
        tool_failure(&response, "usage");
        assert_eq!(
            response["result"]["structuredContent"]["detail"],
            "cursor is unknown, expired, or belongs to another workspace authority"
        );
        assert!(other.product.surface_index_search_seen.is_empty());
    }
}

#[test]
fn oversized_signed_index_search_cursor_projection_is_refused_without_truncation() {
    let owner_cursor = URL_SAFE_NO_PAD.encode(format!(
        "{{\"padding\":\"{}\"}}",
        "x".repeat(26 * 1024 * 3 / 4)
    ));
    assert!((26 * 1024..27 * 1024).contains(&owner_cursor.len()));
    let mut server = ready(Fake {
        surface_index_search_pages: true,
        surface_index_search_owner_cursor: Some(owner_cursor.clone()),
        ..Fake::default()
    });
    let response = call(
        &mut server,
        "backend.index_search",
        &json!({"query":"maven","limit":1,"detail":"summary"}),
    );
    assert_context_bounded(&response);
    assert_eq!(response["isError"], true);
    assert_eq!(response["structuredContent"]["answer"], "fault");
    assert_eq!(response["structuredContent"]["cause"], "oversized");
    assert!(
        !serde_json::to_string(&response)
            .expect("refusal JSON")
            .contains(&owner_cursor)
    );
    assert!(!text_of(&response).contains(&owner_cursor));
}

#[test]
fn index_search_tool_cursor_rejects_context_changes_and_raw_owner_tokens() {
    for mutation in [
        "query",
        "limit",
        "detail",
        "project",
        "authority",
        "tamper",
        "expired",
        "raw",
    ] {
        let mut server = ready(Fake {
            surface_index_search_pages: true,
            ..Fake::default()
        });
        let first = call(
            &mut server,
            "backend.index_search",
            &json!({"query":"maven","limit":1,"detail":"summary"}),
        );
        let cursor = first["structuredContent"]["nextCursor"]
            .as_str()
            .expect("first page carries a signed cursor")
            .to_owned();
        let mut arguments = json!({
            "query": "maven",
            "limit": 1,
            "detail": "summary",
            "cursor": cursor
        });
        match mutation {
            "query" => arguments["query"] = json!("spring"),
            "limit" => arguments["limit"] = json!(2),
            "detail" => arguments["detail"] = json!("full"),
            "project" => server.project = "/other-project".to_owned(),
            "authority" => server.cursor_secret = [8; 32].into(),
            "tamper" => {
                let token = arguments["cursor"].as_str().expect("cursor token");
                let mut changed = token.to_owned();
                let final_byte = changed.pop().expect("MAC byte");
                changed.push(if final_byte == '0' { '1' } else { '0' });
                arguments["cursor"] = json!(changed);
            }
            "expired" => {
                let context_arguments = json!({"query":"maven","limit":1,"detail":"summary"});
                let context = continuation_context(
                    PROJECT,
                    "backend.index_search",
                    context_arguments.as_object().expect("tool arguments"),
                    Detail::Summary,
                );
                arguments["cursor"] = json!(
                    server
                        .sign_cursor_token_at(
                            unix_seconds().saturating_sub(1),
                            FIXTURE_INDEX_CURSOR,
                            &context
                        )
                        .expect("admitted key")
                );
            }
            "raw" => arguments["cursor"] = json!(FIXTURE_INDEX_CURSOR),
            _ => unreachable!(),
        }
        let response = request(
            &mut server,
            "tools/call",
            &json!({"name":"backend.index_search","arguments":arguments}),
        );
        tool_failure(&response, "usage");
        assert_eq!(server.product.surface_index_search_seen.len(), 1);
    }
}

#[test]
fn index_search_cursor_cannot_cross_between_named_and_generic_mcp_tools() {
    let mut named = ready(Fake {
        surface_index_search_pages: true,
        ..Fake::default()
    });
    let named_first = call(
        &mut named,
        "backend.index_search",
        &json!({"query":"maven","limit":1}),
    );
    let named_cursor = named_first["structuredContent"]["nextCursor"]
        .as_str()
        .expect("named tool cursor")
        .to_owned();
    let generic_replay = request(
        &mut named,
        "tools/call",
        &json!({
            "name": SURFACE_TOOL,
            "arguments": {
                "command": {
                    "operation": "index-search",
                    "query": "maven",
                    "limit": 1,
                    "cursor": named_cursor
                }
            }
        }),
    );
    tool_failure(&generic_replay, "usage");
    assert_eq!(named.product.surface_index_search_seen.len(), 1);

    let mut generic = ready(Fake {
        surface_index_search_pages: true,
        ..Fake::default()
    });
    let generic_first = call(
        &mut generic,
        SURFACE_TOOL,
        &json!({"command":{"operation":"index-search","query":"maven","limit":1}}),
    );
    let generic_cursor = generic_first["structuredContent"]["surface"]["data"]["next_cursor"]
        .as_str()
        .expect("generic surface cursor")
        .to_owned();
    let named_replay = request(
        &mut generic,
        "tools/call",
        &json!({
            "name": "backend.index_search",
            "arguments": {"query":"maven","limit":1,"cursor":generic_cursor}
        }),
    );
    tool_failure(&named_replay, "usage");
    assert_eq!(generic.product.surface_index_search_seen.len(), 1);
}

#[test]
fn stale_named_index_search_cursor_is_a_restartable_refusal() {
    let mut server = ready(Fake {
        surface_index_search_pages: true,
        ..Fake::default()
    });
    let first = call(
        &mut server,
        "backend.index_search",
        &json!({"query":"maven","limit":1}),
    );
    let cursor = first["structuredContent"]["nextCursor"]
        .as_str()
        .expect("first page carries a cursor")
        .to_owned();
    server.product.surface_index_search_stale = true;
    let response = call(
        &mut server,
        "backend.index_search",
        &json!({"query":"maven","limit":1,"cursor":cursor}),
    );
    assert_eq!(response["isError"], true);
    assert_eq!(response["structuredContent"]["answer"], "fault");
    assert_eq!(response["structuredContent"]["slug"], "cursor-mismatch");
    assert!(text_of(&response).contains("restart the query"));
    assert_eq!(
        server.product.surface_index_search_seen,
        vec![None, Some(FIXTURE_INDEX_CURSOR.to_owned())]
    );
}

#[test]
fn surface_index_search_cursor_binds_query_limit_project_and_detail() {
    for mutation in [
        "query",
        "limit",
        "detail",
        "project",
        "authority",
        "tamper",
        "expired",
        "raw",
    ] {
        let mut server = ready(Fake {
            surface_index_search_pages: true,
            ..Fake::default()
        });
        let first_command = json!({
            "operation": "index-search",
            "query": "maven",
            "limit": 1
        });
        let first = call(
            &mut server,
            SURFACE_TOOL,
            &json!({
                "command": first_command.clone(),
                "detail": "summary"
            }),
        );
        let cursor = first["structuredContent"]["surface"]["data"]["next_cursor"]
            .as_str()
            .expect("index-search page carries a nested cursor")
            .to_owned();
        let mut command = json!({
            "operation": "index-search",
            "query": "maven",
            "limit": 1,
            "cursor": cursor
        });
        let mut detail = "summary";
        match mutation {
            "query" => command["query"] = json!("spring"),
            "limit" => command["limit"] = json!(2),
            "detail" => detail = "full",
            "project" => server.project = "/other-project".to_owned(),
            "authority" => server.cursor_secret = [8; 32].into(),
            "tamper" => {
                let token = command["cursor"].as_str().expect("cursor token");
                let mut changed = token.to_owned();
                let final_byte = changed.pop().expect("MAC byte");
                changed.push(if final_byte == '0' { '1' } else { '0' });
                command["cursor"] = json!(changed);
            }
            "expired" => {
                let context_arguments_value = json!({"command": first_command.clone()});
                let context_arguments = context_arguments_value
                    .as_object()
                    .expect("surface arguments");
                let context =
                    continuation_context(PROJECT, SURFACE_TOOL, context_arguments, Detail::Summary);
                command["cursor"] = json!(
                    server
                        .sign_cursor_token_at(
                            unix_seconds().saturating_sub(1),
                            FIXTURE_INDEX_CURSOR,
                            &context
                        )
                        .expect("admitted key")
                );
            }
            "raw" => command["cursor"] = json!(FIXTURE_INDEX_CURSOR),
            _ => unreachable!(),
        }
        let response = request(
            &mut server,
            "tools/call",
            &json!({
                "name": SURFACE_TOOL,
                "arguments": {"command": command, "detail": detail}
            }),
        );
        tool_failure(&response, "usage");
        assert_eq!(server.product.surface_index_search_seen.len(), 1);
    }
}

#[test]
fn stale_surface_index_search_cursor_is_reported_as_restartable() {
    let mut first_server = ready(Fake {
        surface_index_search_pages: true,
        ..Fake::default()
    });
    let first = call(
        &mut first_server,
        SURFACE_TOOL,
        &json!({
            "command": {
                "operation": "index-search",
                "query": "maven",
                "limit": 1
            }
        }),
    );
    let cursor = first["structuredContent"]["surface"]["data"]["next_cursor"]
        .as_str()
        .expect("first page carries a cursor")
        .to_owned();
    let mut stale_owner = ready(Fake {
        surface_index_search_pages: true,
        surface_index_search_stale: true,
        ..Fake::default()
    });
    let response = request(
        &mut stale_owner,
        "tools/call",
        &json!({
            "name": SURFACE_TOOL,
            "arguments": {
                "command": {
                    "operation": "index-search",
                    "query": "maven",
                    "limit": 1,
                    "cursor": cursor
                }
            }
        }),
    );
    let fault = tool_failure(&response, "cursor-mismatch");
    assert_eq!(fault["cause"], "moved");
    assert!(text_of(&response["result"]).contains("restart the query"));
}

#[test]
fn stale_graph_roots_are_reported_as_a_restartable_cursor_error() {
    let mut server = ready(Fake {
        graph_continue: true,
        ..Fake::default()
    });
    let first = call(
        &mut server,
        QUERY_TOOL,
        &json!({"query":"{ Declaration { coordinate @output } }","limit":1}),
    );
    let cursor = first["structuredContent"]["nextCursor"]
        .as_str()
        .expect("first page carries a cursor")
        .to_owned();
    server.product.stale_cursor = true;
    let response = request(
        &mut server,
        "tools/call",
        &json!({
            "name": QUERY_TOOL,
            "arguments": {
                "query":"{ Declaration { coordinate @output } }",
                "limit":1,
                "cursor":cursor
            }
        }),
    );
    let fault = tool_failure(&response, "cursor-mismatch");
    assert_eq!(fault["cause"], "moved");
    assert!(text_of(&response["result"]).contains("restart the query"));
}

#[test]
fn high_fanout_graph_and_surface_pages_refuse_atomically() {
    let rows = (0..256)
        .map(|index| {
            let mut fields = std::collections::BTreeMap::new();
            fields.insert(
                "coordinate".to_owned(),
                GraphValue::String(format!("pkg::module_{index}::{}", "λאב".repeat(96))),
            );
            fields.insert(
                "summary".to_owned(),
                GraphValue::String("high fanout graph record ".repeat(32)),
            );
            GraphQueryRow::new(fields).expect("admitted graph row")
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let mut server = ready(Fake {
        graph_rows: Some(rows),
        ..Fake::default()
    });
    let graph = call(
        &mut server,
        QUERY_TOOL,
        &json!({"query":"{ Declaration { coordinate @output } }"}),
    );
    assert_context_bounded(&graph);
    assert_eq!(graph["isError"], true);
    assert_eq!(graph["structuredContent"]["cause"], "oversized");

    let references = (0..256)
        .map(|index| backend_library::ReferenceRecord {
            site: backend_library::ProductText::new(format!(
                "pkg::caller_{index}::{}",
                "x".repeat(160)
            ))
            .expect("site text"),
            target: backend_library::SemanticLinkTarget::Local {
                declaration: backend_library::SemanticDeclarationIdentity {
                    family: [1; 16],
                    variant: [2; 16],
                },
            },
            relation: backend_library::SemanticLinkKind::Calls,
            evidence: backend_library::SemanticLinkEvidence {
                confidence: backend_library::SemanticConfidence::Compiler,
                source: Some(backend_library::SemanticSourceSpan {
                    file: backend_library::ProductText::new("src/main.rs").expect("path text"),
                    start: index,
                    end: index + 1,
                }),
            },
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let mut server = ready(Fake {
        surface_reply: Some(SurfaceReply::References {
            target: backend_library::ProductText::new("pkg::semantic::callee")
                .expect("target text"),
            references,
        }),
        ..Fake::default()
    });
    let surface = call(
        &mut server,
        SURFACE_TOOL,
        &json!({
            "command": serde_json::to_value(SurfaceCommand::References {
                target: backend_library::ProductText::new("pkg::semantic::callee")
                    .expect("target text")
            }).expect("encode references command")
        }),
    );
    assert_context_bounded(&surface);
    assert_eq!(surface["isError"], true);
    assert_eq!(surface["structuredContent"]["cause"], "oversized");
}

#[test]
fn recognized_tools_share_typed_missing_input_results_before_any_owner_call() {
    let mut server = ready(Fake::default());
    let listed = request(&mut server, "tools/list", &json!({}));
    let mut required_tools = listed["result"]["tools"]
        .as_array()
        .expect("tool catalog")
        .iter()
        .filter(|tool| {
            tool["inputSchema"]["required"]
                .as_array()
                .is_some_and(|required| !required.is_empty())
        })
        .map(|tool| tool["name"].as_str().expect("tool name").to_owned())
        .collect::<Vec<_>>();
    required_tools.extend([QUERY_TOOL.to_owned(), SURFACE_TOOL.to_owned()]);
    assert!(
        required_tools.len() >= 8,
        "exercise advertised and catalog-only required operands"
    );
    for name in required_tools {
        let response = request(
            &mut server,
            "tools/call",
            &json!({"name":name, "arguments":{}}),
        );
        let fault = tool_failure(&response, "usage");
        assert_eq!(fault["cause"], "malformed", "{name}: {response}");
        assert!(
            !fault["detail"]
                .as_str()
                .expect("required input guidance")
                .is_empty()
        );
        assert_eq!(server.product.probe_calls, 0, "{name}");
        assert_eq!(server.product.graph_query_calls, 0, "{name}");
        assert!(server.product.index_paths.is_empty(), "{name}");
        assert!(server.product.surface_commands.is_empty(), "{name}");
    }
}

#[test]
fn malformed_call_tool_envelopes_and_unknown_routes_remain_rpc_errors() {
    let mut server = ready(Fake::default());
    for params in [
        json!([]),
        json!({}),
        json!({"name":7}),
        json!({"name":"backend.index", "arguments":null}),
        json!({"name":"backend.index", "arguments":[]}),
        json!({"name":"backend.index", "arguments":"bad envelope"}),
        json!({"name":"backend.not-a-tool", "arguments":{}}),
    ] {
        let response = request(&mut server, "tools/call", &params);
        assert_eq!(response["error"]["code"], -32602, "{params}: {response}");
        assert!(response.get("result").is_none(), "{params}: {response}");
        assert_eq!(response["id"], 9);
        assert_eq!(server.product.probe_calls, 0);
        assert!(server.product.surface_commands.is_empty());
    }
}

#[test]
fn authenticated_continuation_decode_preserves_domain_and_server_fault_distinctions() {
    for (error, slug, protocol) in [
        (ClientError::StaleCursor, "cursor-mismatch", false),
        (ClientError::CursorMismatch, "cursor-mismatch", false),
        (
            ClientError::Transport(backend_replication::ReplicationError::MessageTooLarge),
            "transport",
            false,
        ),
        (
            ClientError::Protocol("retained owner proof is corrupt".to_owned()),
            "protocol",
            true,
        ),
        (
            ClientError::RequestMismatch {
                expected: 1,
                observed: 2,
            },
            "request-mismatch",
            true,
        ),
        (ClientError::FreshnessMismatch, "freshness", true),
        (ClientError::IncoherentView, "incoherent-view", true),
    ] {
        let mut server = ready(Fake {
            page_catalog: Some(paging_catalog(3)),
            ..Fake::default()
        });
        let first = call(
            &mut server,
            "backend.search",
            &json!({"query":"Session", "limit":1}),
        );
        let cursor = first["structuredContent"]["nextCursor"]
            .as_str()
            .expect("actual catalog continuation")
            .to_owned();
        server.product.continuation_error = Some(error);
        let response = request(
            &mut server,
            "tools/call",
            &json!({"name":"backend.search", "arguments":{"query":"Session", "limit":1, "cursor":cursor}}),
        );
        if protocol {
            assert_eq!(response["error"]["code"], -32603, "{response}");
            assert_eq!(response["error"]["data"]["structuredContent"]["slug"], slug);
            assert!(response.get("result").is_none());
        } else {
            tool_failure(&response, slug);
        }
        assert_eq!(
            server.product.page_limits,
            [1],
            "a refused cursor never executes another page"
        );
    }
}

#[test]
fn generic_surface_dependency_refusal_keeps_exact_package_action_and_typed_fault() {
    let package = PackageReference::parse("pkg:npm/react@19.1.0").expect("package");
    let command = SurfaceCommand::Dependencies {
        package: package.clone(),
    };
    let mut server = ready(Fake {
        surface_reply: Some(SurfaceReply::Dependencies(
            backend_library::DependencyFacts::Unavailable(
                backend_library::ProductText::from_static("dependency evidence was not captured"),
            ),
        )),
        ..Fake::default()
    });
    let result = call(&mut server, SURFACE_TOOL, &json!({"command":command}));
    assert_eq!(result["isError"], true);
    let fault = &result["structuredContent"]["fault"];
    assert_eq!(fault["slug"], "lane-unavailable");
    assert_eq!(fault["cause"], "not-captured");
    assert_eq!(fault["operand"], package.as_str());
    assert_eq!(
        fault["call"],
        json!({"name":"backend.package", "arguments":{"package":package.as_str()}})
    );
    assert!(text_of(&result).contains(package.as_str()));
    assert_eq!(server.product.surface_commands, [command]);
}

#[test]
fn direct_and_generic_graph_routes_preserve_typed_domain_and_protocol_failures() {
    for failure in [
        backend_library::CommandFailure::NotFound,
        backend_library::CommandFailure::WrongBasis {
            expected: view_state_root(&[("fixture".to_owned(), "current-view".to_owned())]).into(),
            observed: view_state_root(&[("fixture".to_owned(), "stale-view".to_owned())]).into(),
        },
    ] {
        let expected = if matches!(failure, backend_library::CommandFailure::NotFound) {
            "not-found"
        } else {
            "wrong-basis"
        };
        let expected_operand = match &failure {
            backend_library::CommandFailure::WrongBasis { expected, observed } => format!(
                "{} (owner holds {})",
                &encode_id(observed.as_bytes())[..8],
                &encode_id(expected.as_bytes())[..8]
            ),
            _ => DECLARATION.to_owned(),
        };
        let mut direct = ready(Fake {
            graph_failure: Some(failure.clone()),
            ..Fake::default()
        });
        let response = request(
            &mut direct,
            "tools/call",
            &json!({"name":"backend.graph", "arguments":{"coordinate":DECLARATION, "limit":1}}),
        );
        let fault = tool_failure(&response, expected);
        assert_eq!(fault["operand"], expected_operand);
        if expected == "not-found" {
            assert_eq!(
                fault["call"],
                json!({"name":"backend.search", "arguments":{"query":"ferris"}})
            );
        }
        let mut generic = ready(Fake {
            graph_query_error: Some(ClientError::CommandFailed(failure)),
            ..Fake::default()
        });
        let response = request(
            &mut generic,
            "tools/call",
            &json!({"name":QUERY_TOOL, "arguments":{"query":"{ Declaration { coordinate @output } }"}}),
        );
        tool_failure(&response, expected);
    }
    let mut corrupt = ready(Fake {
        graph_failure: Some(backend_library::CommandFailure::IncoherentView(
            "corrupt selected relation".to_owned(),
        )),
        ..Fake::default()
    });
    let response = request(
        &mut corrupt,
        "tools/call",
        &json!({"name":"backend.graph", "arguments":{"coordinate":DECLARATION}}),
    );
    assert_eq!(response["error"]["code"], -32603);
    assert_eq!(
        response["error"]["data"]["structuredContent"]["slug"],
        "incoherent-view"
    );
    assert!(response.get("result").is_none());
}

#[test]
fn partial_terminal_keeps_error_flag_exact_ticket_partition_and_receipt_in_the_same_mcp_wire_result()
 {
    use backend_library::{
        IndexJobOutcome, IndexJobPartialPublication, IndexOperationFailureReason,
        IndexOperationKey, IndexOperationObservation, IndexOperationProfileRefusal,
        IndexOperationPublicationReceipt, IndexOperationSemanticCoverage,
        IndexOperationSemanticProfileState, IndexOperationSemanticUnavailableReason,
        IndexOperationSourceCaptureReceipt, IndexOperationSourceProfile, IndexOperationState,
        IndexOperationStatus, IndexSourceCaptureSummary, ProductText, SemanticLanguageProfile,
    };
    // This tests closed serialization and caller admission, not an actual
    // compiler generation. The view carries no complete-coverage authority.
    let package = PackageReference::parse("/abs/mixed-docs").expect("package");
    let ticket = IndexJobTicket::new(
        std::num::NonZeroU64::new(31).expect("ticket"),
        [19; 16],
        package.clone(),
    );
    let py = SemanticLanguageProfile::from_name("python").expect("Python");
    let ts = SemanticLanguageProfile::from_name("typescript").expect("TypeScript");
    let mut profiles = vec![
        IndexOperationSourceProfile {
            profile: py,
            source_version: [4; 32],
            input_digest: [5; 32],
            observation_sequence: 2,
            source_count: 236,
            state: IndexOperationSemanticProfileState::Published {
                generation: [6; 32],
                coverage: IndexOperationSemanticCoverage::Complete,
            },
        },
        IndexOperationSourceProfile {
            profile: ts,
            source_version: [4; 32],
            input_digest: [7; 32],
            observation_sequence: 3,
            source_count: 626,
            state: IndexOperationSemanticProfileState::Unavailable {
                reason: IndexOperationSemanticUnavailableReason::Rejected,
            },
        },
    ];
    profiles.sort_by_key(|profile| profile.profile);
    let view = root(Vec::new());
    let partial = IndexJobPartialPublication {
        package: package.clone(),
        receipt: IndexOperationPublicationReceipt::from_published_view(
            Some([8; 32]),
            [9; 32],
            [10; 32],
            3,
            &view,
            backend_library::Cursor::for_view_root(&view),
        )
        .expect("structurally admitted receipt"),
        source_capture: IndexSourceCaptureSummary {
            producer_package: package,
            request_identity: [1; 32],
            commit_identity: [2; 32],
            workspace_root: [3; 32],
            workspace_sequence: 2,
            profiles: profiles.into_boxed_slice(),
        },
        refused_profiles: vec![IndexOperationProfileRefusal {
            profile: ts,
            reason: IndexOperationSemanticUnavailableReason::Rejected,
            compiler_failure: Some(setup_compiler_failure()),
        }]
        .into_boxed_slice(),
    };
    partial.admit().expect("complete typed partition");
    let observation = IndexJobObservation::Terminal(IndexJobTerminal {
        ticket: ticket.clone(),
        outcome: IndexJobOutcome::PartiallyPublished(partial.clone()),
    });
    let reply = SurfaceReply::IndexProgress(observation.clone());
    let command = SurfaceCommand::IndexProgress {
        ticket: ticket.clone(),
        after_sequence: 0,
    };
    let dto = ReplyDto::new(701, CommandReply::Surface(reply.clone()));
    let bytes = serde_json::to_vec(&dto).expect("closed backend DTO");
    let decoded =
        backend_library::decode_reply_body(&bytes).expect("genuine closed backend wire decode");
    backend_library::admit_reply(
        &backend_library::CommandDto::new(701, backend_library::Command::Surface(command)),
        &decoded,
    )
    .expect("exact caller ticket and partial payload admission");
    for detail in ["summary", "full"] {
        let mut server = ready(Fake {
            surface_reply: Some(reply.clone()),
            ..Fake::default()
        });
        let wire = request(
            &mut server,
            "tools/call",
            &json!({"name":INDEX_PROGRESS_TOOL, "arguments":{"ticket":ticket, "after_sequence":0, "detail":detail}}),
        );
        assert!(wire.get("error").is_none(), "{wire}");
        assert_eq!(wire["result"]["isError"], true);
        let content = &wire["result"]["structuredContent"];
        assert_eq!(
            content["surface"]["data"],
            serde_json::to_value(&observation).expect("exact observation")
        );
        assert_eq!(
            content["surface"]["index_job"],
            serde_json::to_value(backend_present::IndexJobProjection::Progress(
                observation.clone()
            ))
            .expect("exact job projection")
        );
        assert_eq!(content["fault"]["slug"], "partially-published");
        assert_eq!(
            content["fault"]["partial_publication"],
            serde_json::to_value(&partial)
                .expect("exact receipt, partition, refusals and source tuple")
        );
        assert_eq!(
            content["fault"]["partial_publication"]["receipt"],
            serde_json::to_value(&partial.receipt).expect("receipt")
        );
        let text = text_of(&wire["result"]);
        assert!(text.contains("python: published 236 source files with Complete coverage"));
        assert!(text.contains("typescript: unavailable"));
        assert!(text.contains("NUDOX_TSC"));
        assert_context_bounded(&wire);
        assert_eq!(server.product.surface_commands.len(), 1);
    }
    let mut compact_server = ready(Fake {
        surface_reply: Some(reply.clone()),
        ..Fake::default()
    });
    let compact = call(
        &mut compact_server,
        INDEX_PROGRESS_TOOL,
        &json!({
            "ticket":ticket_value(&ticket),
            "after_sequence":0,
            "compact_receipt":true
        }),
    );
    assert_eq!(compact["isError"], true);
    let compact_data = serde_json::to_value(&observation).expect("exact partial observation");
    assert_compact_index_job_receipt(&compact, &compact_data["data"]);
    let compact_value = &compact["structuredContent"]["surface"]["index_job"]["value"]["detail"];
    assert_eq!(
        compact_value["ticket"]["owner_epoch"],
        serde_json::to_value([19_u8; 16]).expect("owner epoch")
    );
    let profiles = compact_value["outcome"]["detail"]["source_capture"]["profiles"]
        .as_array()
        .expect("all source profiles retained");
    assert!(
        profiles
            .iter()
            .any(|profile| { profile["state"]["detail"]["coverage"]["coverage"] == "complete" })
    );
    assert_eq!(
        compact_value["outcome"]["detail"]["refused_profiles"][0]["reason"],
        "rejected"
    );
    assert_context_bounded(&compact);
    let expected_cause = backend_present::product_view(&reply)
        .fault()
        .expect("legacy partial fault")
        .cause()
        .sentence()
        .to_owned();
    let key = IndexOperationKey::from_bytes([0x41; 32]).expect("durable key");
    let capture = IndexOperationSourceCaptureReceipt::from_checked_parts(
        key,
        partial.source_capture.commit_identity,
        partial.source_capture.workspace_root,
        partial.source_capture.workspace_sequence,
        partial.source_capture.profiles.clone(),
    )
    .expect("exact durable source receipt");
    let partial_status = IndexOperationStatus::new(
        key,
        partial.package.clone(),
        CompileExecutionIntent::Interactive,
        IndexOperationState::PartiallyPublished {
            receipt: partial.receipt.clone(),
            refused_profiles: partial.refused_profiles.clone(),
        },
    )
    .with_source_capture(Some(capture));
    for (status, fault_slug) in [
        (partial_status.clone(), Some("partially-published")),
        (
            IndexOperationStatus::new(
                key,
                partial.package.clone(),
                CompileExecutionIntent::Interactive,
                IndexOperationState::Accepted,
            ),
            None,
        ),
        (
            IndexOperationStatus::new(
                key,
                partial.package.clone(),
                CompileExecutionIntent::Interactive,
                IndexOperationState::Failed {
                    reason: IndexOperationFailureReason::Cancelled,
                    detail: ProductText::from_static("cancelled before commit"),
                    compiler_failure: None,
                },
            ),
            Some("rejected"),
        ),
    ] {
        let operation = IndexOperationObservation::Known(status);
        for started in [false, true] {
            let (command, reply, name, arguments) = if started {
                (
                    SurfaceCommand::IndexOperationStart {
                        operation_key: key,
                        package: partial.package.clone(),
                        execution_intent: CompileExecutionIntent::Interactive,
                    },
                    SurfaceReply::IndexOperationStarted(operation.clone()),
                    INDEX_START_TOOL,
                    json!({"package":partial.package.as_str(), "operation_key":key.to_hex()}),
                )
            } else {
                (
                    SurfaceCommand::IndexOperationStatus { operation_key: key },
                    SurfaceReply::IndexOperationStatus(operation.clone()),
                    INDEX_PROGRESS_TOOL,
                    json!({"operation_key":key.to_hex()}),
                )
            };
            let dto = ReplyDto::new(702, CommandReply::Surface(reply.clone()));
            let bytes = serde_json::to_vec(&dto).expect("actual durable backend wire");
            let decoded = backend_library::decode_reply_body(&bytes).expect("durable wire decode");
            backend_library::admit_reply(
                &backend_library::CommandDto::new(702, backend_library::Command::Surface(command)),
                &decoded,
            )
            .expect("exact operation, package and intent admission before projection");
            for detail in ["summary", "full"] {
                let mut arguments = arguments.clone();
                arguments["detail"] = json!(detail);
                let mut server = ready(Fake {
                    surface_reply: Some(reply.clone()),
                    ..Fake::default()
                });
                let wire = request(
                    &mut server,
                    "tools/call",
                    &json!({"name":name, "arguments":arguments}),
                );
                assert!(wire.get("error").is_none(), "{wire}");
                assert_eq!(wire["id"], 9);
                assert_eq!(wire["result"]["isError"], fault_slug.is_some(), "{wire}");
                let content = &wire["result"]["structuredContent"];
                if fault_slug.is_none() {
                    assert_eq!(
                        content["index_poll"]["call"],
                        json!({
                            "name":"backend.index_progress", "arguments":{"operation_key":key.to_hex(), "detail":detail}
                        })
                    );
                } else {
                    assert!(
                        content.get("index_poll").is_none(),
                        "terminal receipts do not need another poll"
                    );
                    assert!(!text_of(&wire["result"]).contains("backend.index_progress"));
                }
                assert_eq!(
                    content["surface"]["data"],
                    serde_json::to_value(&operation).expect("complete exact operation")
                );
                assert_eq!(
                    content["surface"]["data"]["detail"]["operation_key"],
                    key.to_hex()
                );
                assert_eq!(
                    content["surface"]["data"]["detail"]["request_digest"],
                    "bd8be50867ceba6df5959b6a6fdb3651b3f510574692c386858435e6e3c26897"
                );
                if fault_slug == Some("partially-published") {
                    let fault = backend_present::product_view(&reply);
                    assert_eq!(
                        fault
                            .fault()
                            .expect("durable partial fault")
                            .cause()
                            .sentence(),
                        expected_cause
                    );
                    assert_eq!(content["fault"]["slug"], "partially-published");
                    assert!(
                        content["fault"]["partial_publication"].is_null(),
                        "no manufactured legacy source tuple"
                    );
                    assert_eq!(
                        content["surface"]["data"]["detail"]["source_capture"],
                        serde_json::to_value(&partial_status.source_capture)
                            .expect("retained exact capture")
                    );
                    let text = text_of(&wire["result"]);
                    assert!(
                        text.contains("python: published 236 source files with Complete coverage")
                    );
                    assert!(text.contains("typescript: unavailable"));
                    assert!(text.contains("Set NUDOX_TSC"));
                    assert!(text.contains(&key.to_hex()));
                } else if fault_slug == Some("rejected") {
                    // A terminal cancellation failed the indexing goal. It
                    // retains its exact operation, but claims no publication,
                    // source partition, or compiler failure. Its retry starts
                    // a fresh attempt rather than polling the consumed key.
                    let fault = &content["fault"];
                    assert_eq!(fault["slug"], "rejected");
                    assert_eq!(fault["operand"], key.to_hex());
                    assert_eq!(fault["cause"], "refused");
                    assert_eq!(
                        fault["detail"],
                        "index operation was cancelled before publication: cancelled before commit"
                    );
                    assert!(fault["partial_publication"].is_null());
                    assert!(fault["compiler_failure"].is_null());
                    assert!(content["surface"]["data"]["detail"]["source_capture"].is_null());
                    assert_eq!(
                        content["surface"]["data"]["detail"]["state"],
                        json!({"state":"failed", "detail":{"reason":"cancelled", "detail":"cancelled before commit"}})
                    );
                    assert_eq!(
                        fault["call"],
                        json!({"name":"backend.index", "arguments":{"path":"/abs/mixed-docs", "execution_intent":"interactive"}})
                    );
                    assert_eq!(
                        fault["shell"],
                        "nudox add /abs/mixed-docs --execution-intent interactive"
                    );
                    let text = text_of(&wire["result"]);
                    assert!(text.contains("index operation cancelled: cancelled before commit"));
                    assert!(text.contains(&key.to_hex()));
                    assert!(!text.contains("Complete coverage"));
                    assert!(!text.contains("typescript: unavailable"));
                    assert!(!text.contains("NUDOX_TSC"));
                } else {
                    assert!(content["fault"].is_null());
                    assert_eq!(
                        content["surface"]["data"]["detail"]["state"],
                        json!({"state":"accepted"})
                    );
                    assert!(content["surface"]["data"]["detail"]["source_capture"].is_null());
                }
                assert_context_bounded(&wire);
            }
        }
    }
}

/// Real private state for deferred cursor-authority admission. No owner is
/// started: token authentication itself is the behavior under test.
struct LazyAuthorityFixture {
    root: std::path::PathBuf,
    project: std::path::PathBuf,
    paths: backend_runtime::WorkspacePaths,
}

impl LazyAuthorityFixture {
    fn new() -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("fixture clock")
            .as_nanos();
        let base = if cfg!(unix) {
            std::path::PathBuf::from("/tmp")
        } else {
            std::env::temp_dir()
        };
        let name = format!("mcp-lazy-{}-{nonce:x}", std::process::id());
        let root = base.join(&name);
        let project = base.join(format!("{name}-project"));
        // /tmp is deliberately other-writable: it is not an admissible
        // application-data parent. Own a private fixture parent first, then
        // let the real WorkspacePaths admission create its state child.
        std::fs::create_dir(&root).expect("owned fixture parent");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                .expect("private fixture parent permissions");
        }
        #[cfg(windows)]
        backend_platform::win32::security::restrict_to_current_user(&root)
            .expect("private fixture parent ACL");
        std::fs::create_dir(&project).expect("fixture project");
        let paths = backend_runtime::WorkspacePaths::discover(
            Some(project.clone()),
            Some(root.join("state")),
            None,
        )
        .expect("selected fixture workspace");
        Self {
            root,
            project,
            paths,
        }
    }
}

impl Drop for LazyAuthorityFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
        let _ = std::fs::remove_dir_all(&self.project);
    }
}

#[test]
fn lazy_cursor_authority_is_durable_once_and_replayable_in_a_fresh_server() {
    let fixture = LazyAuthorityFixture::new();
    let authority = CursorAuthority::workspace(&fixture.paths);
    let first = Server::with_authority(Fake::default(), PROJECT.to_owned(), authority.clone());
    assert!(!fixture.paths.data().exists());
    let token = first
        .sign_cursor_token(FIXTURE_INDEX_CURSOR, b"same-request")
        .expect("first cursor durably admits its authority");
    let persisted = read_authority_secret(fixture.paths.authority_secret()).expect("durable key");
    assert_eq!(authority.key().expect("same admitted authority"), persisted);
    let second = Server::with_authority(
        Fake::default(),
        PROJECT.to_owned(),
        CursorAuthority::workspace(&fixture.paths),
    );
    assert_eq!(
        second
            .verify_cursor_token(&token, b"same-request")
            .expect("cold process reads durable authority")
            .as_deref(),
        Some(FIXTURE_INDEX_CURSOR)
    );
    assert_eq!(
        read_authority_secret(fixture.paths.authority_secret()).expect("unchanged authority"),
        persisted
    );
}

#[test]
fn lazy_cursor_authority_retries_after_repair_without_process_key_fallback() {
    let fixture = LazyAuthorityFixture::new();
    std::fs::write(fixture.paths.data(), b"not a workspace directory")
        .expect("unavailable durable workspace");
    let authority = CursorAuthority::workspace(&fixture.paths);
    let server = Server::with_authority(Fake::default(), PROJECT.to_owned(), authority.clone());
    let first = server
        .sign_cursor_token(FIXTURE_INDEX_CURSOR, b"query")
        .expect_err("a durable authority refusal must not mint an ephemeral cursor");
    assert_eq!(first.kind, "transport");
    assert!(
        first
            .detail
            .as_deref()
            .is_some_and(|detail| !detail.is_empty())
    );
    let repeated = server
        .sign_cursor_token(FIXTURE_INDEX_CURSOR, b"query")
        .expect_err("the unchanged failure retains its original cause");
    assert_eq!(repeated.detail, first.detail);
    std::fs::remove_file(fixture.paths.data()).expect("retire unavailable fixture");
    let token = server
        .sign_cursor_token(FIXTURE_INDEX_CURSOR, b"query")
        .expect("repaired workspace can admit its first durable authority");
    let persisted = read_authority_secret(fixture.paths.authority_secret()).expect("durable key");
    assert_eq!(authority.key().expect("admitted authority"), persisted);
    assert_eq!(
        server
            .verify_cursor_token(&token, b"query")
            .expect("durable token")
            .as_deref(),
        Some(FIXTURE_INDEX_CURSOR)
    );
}

#[test]
fn simultaneous_lazy_cursor_admission_uses_one_durable_identity() {
    let fixture = LazyAuthorityFixture::new();
    let shared = CursorAuthority::workspace(&fixture.paths);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let workers = (0..8)
        .map(|index| {
            let authority = if index % 2 == 0 {
                shared.clone()
            } else {
                CursorAuthority::workspace(&fixture.paths)
            };
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                authority.key().expect("simultaneous durable admission")
            })
        })
        .collect::<Vec<_>>();
    let persisted = workers
        .into_iter()
        .map(|worker| worker.join().expect("admission worker"))
        .collect::<Vec<_>>();
    let durable = read_authority_secret(fixture.paths.authority_secret()).expect("durable key");
    assert!(persisted.into_iter().all(|key| key == durable));
    assert_eq!(shared.key().expect("stable shared authority"), durable);
}

#[test]
fn malformed_or_expired_cursor_does_not_admit_a_workspace_authority() {
    let fixture = LazyAuthorityFixture::new();
    let server = Server::with_authority(
        Fake::default(),
        PROJECT.to_owned(),
        CursorAuthority::workspace(&fixture.paths),
    );
    for token in [
        "not-a-cursor",
        "mcp1-0-b3duZXI-00000000000000000000000000000000",
    ] {
        assert!(
            server
                .verify_cursor_token(token, b"query")
                .expect("invalid input")
                .is_none()
        );
    }
    assert!(!fixture.paths.data().exists());
}

#[test]
fn cold_cursor_verification_never_initializes_authority_for_a_forged_token() {
    let fixture = LazyAuthorityFixture::new();
    let authority = CursorAuthority::workspace(&fixture.paths);
    let server = Server::with_authority(Fake::default(), PROJECT.to_owned(), authority.clone());
    let future = unix_seconds().saturating_add(900);
    for token in [
        format!("mcp1-{future}-cGMxLWZvcmdlZA-{}", "00".repeat(16)),
        format!("mcp1-{future}-cGMxLWZvcmdlZA-not-a-canonical-mac"),
        format!("mcp1-{future}-invalid*base64-{}", "00".repeat(16)),
        cursor_wrapper_vector(
            "mcp2",
            &future.to_string(),
            FIXTURE_INDEX_CURSOR,
            [7; 32],
            b"query",
        ),
        format!("mcp2-{future}-{FIXTURE_INDEX_CURSOR}-not-a-canonical-mac"),
    ] {
        assert!(
            server
                .verify_cursor_token(&token, b"query")
                .expect("read-only verification")
                .is_none()
        );
        assert!(
            !fixture.paths.data().exists(),
            "untrusted continuation cannot provision an authority"
        );
    }
    assert!(
        authority
            .verification_key()
            .expect("missing durable key")
            .is_none()
    );
    let token = server
        .sign_cursor_token(FIXTURE_INDEX_CURSOR, b"query")
        .expect("actual signing request can admit a durable key");
    assert_eq!(
        server
            .verify_cursor_token(&token, b"query")
            .expect("post-sign verification")
            .as_deref(),
        Some(FIXTURE_INDEX_CURSOR)
    );
    assert!(fixture.paths.authority_secret().is_file());
}

#[test]
fn query_preparation_is_a_typed_transient_tool_result_with_exact_retry_not_index_failure() {
    let root = backend_library::view_state_root(&[]);
    let basis = backend_library::ViewRevision::from(root);
    for state in [
        backend_library::QueryPreparationState::Preparing,
        backend_library::QueryPreparationState::Retiring,
    ] {
        let fault = backend_present::Fault::from_command_failure(
            &backend_library::CommandFailure::QueryPreparation { basis, state },
            backend_present::Operand::Text("authentication".to_owned()),
        )
        .with_affordance(backend_present::Affordance::RetrySearch {
            text: "authentication".to_owned(),
            limit: 17,
        });
        let result = RpcError::from_fault(&fault)
            .into_tool_result()
            .expect("typed workload readiness is a domain result");
        assert_eq!(result["isError"], true);
        let dto = &result["structuredContent"];
        assert_eq!(dto["slug"], "query-preparation");
        assert_eq!(
            dto["query_preparation"]["basis"],
            backend_library::encode_id(root.as_bytes())
        );
        assert_eq!(dto["query_preparation"]["state"], state.as_str());
        assert_eq!(
            dto["call"],
            json!({"name":"backend.search","arguments":{"query":"authentication","limit":17}})
        );
        assert_eq!(dto["shell"], "nudox search authentication --limit 17");
        assert!(dto.get("compiler_failure").is_none_or(Value::is_null));
        assert!(dto.get("partial_publication").is_none_or(Value::is_null));
        assert!(fault.cause().sentence().contains("indexing has not failed"));
    }
}

#[cfg(unix)]
#[test]
fn stdio_reader_cancels_an_active_call_then_allows_completed_id_reuse() {
    struct Blocking {
        inner: Fake,
        context: Option<RequestContext>,
        admitted: std::sync::mpsc::SyncSender<()>,
    }
    impl Engine for Blocking {
        fn revision(&mut self) -> Result<ViewStateRoot, ClientError> {
            self.inner.revision()
        }
        fn health(&mut self) -> Result<HealthReport, ClientError> {
            self.inner.health()
        }
        fn probe(&mut self, probe: Probe<'_>) -> Result<ReplyDto, ClientError> {
            if matches!(probe, Probe::Search { .. }) {
                self.admitted.send(()).expect("query has actually started");
                let context = self
                    .context
                    .as_ref()
                    .expect("request context reaches product");
                let reason = futures_executor::block_on(context.cancelled());
                return Err(ClientError::RequestCancelled(reason));
            }
            self.inner.probe(probe)
        }
        fn probe_page_window(
            &mut self,
            probe: Probe<'_>,
            continuation: Option<PageContinuation>,
            window: backend_library::QueryWindow,
        ) -> Result<ReplyDto, ClientError> {
            if matches!(probe, Probe::Search { .. }) {
                self.probe(probe)
            } else {
                self.inner.probe_page_window(probe, continuation, window)
            }
        }
        fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
            self.inner.surface(command)
        }
    }
    impl Product for Blocking {
        fn set_request_context(
            &mut self,
            context: Option<RequestContext>,
        ) -> Result<(), ClientError> {
            self.context = context;
            self.context.as_ref().map_or(Ok(()), RequestContext::check)
        }
        fn encode_continuation(
            &mut self,
            continuation: PageContinuation,
        ) -> Result<String, ClientError> {
            self.inner.encode_continuation(continuation)
        }
        fn decode_continuation(&mut self, token: &str) -> Result<PageContinuation, ClientError> {
            self.inner.decode_continuation(token)
        }
        fn graph_query(
            &mut self,
            input: AdmittedGraphQueryInput,
            limit: u16,
            continuation: Option<PageContinuation>,
        ) -> Result<GraphQueryPage, ClientError> {
            self.inner.graph_query(input, limit, continuation)
        }
    }
    use std::io::{BufRead as _, Write as _};
    use std::time::Duration;
    let (mut input, server_input) = std::os::unix::net::UnixStream::pair().expect("client input");
    let (output, mut server_output) =
        std::os::unix::net::UnixStream::pair().expect("client output");
    server_input
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("fixture reader bound");
    output
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("fixture response bound");
    let mut output = std::io::BufReader::new(output);
    let (admitted, admission) = std::sync::mpsc::sync_channel(1);
    let server = Server::with_authority(
        Blocking {
            inner: Fake::default(),
            context: None,
            admitted,
        },
        PROJECT.to_owned(),
        [7; 32],
    );
    let serving = std::thread::spawn(move || {
        serve_stdio_server(
            server,
            std::io::BufReader::new(server_input),
            &mut server_output,
        )
    });
    writeln!(input, "{}", json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"cancellation-law","version":"1"}}})).expect("initialize");
    let mut line = String::new();
    output.read_line(&mut line).expect("initialize reply");
    let initialized = assert_stdio_line_accounting(&line);
    assert_eq!(initialized["id"], 1);
    assert!(initialized["result"]["serverInfo"].is_object());
    assert!(initialized.get("error").is_none());
    writeln!(
        input,
        "{}",
        json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )
    .expect("acknowledgement");
    writeln!(input, "{}", json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"backend.search","arguments":{"query":"blocked query"}}})).expect("query");
    admission
        .recv_timeout(Duration::from_secs(1))
        .expect("query starts before cancellation arrives");
    writeln!(
        input,
        "{}",
        json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":7}})
    )
    .expect("cancel while actor is blocked");
    line.clear();
    output.read_line(&mut line).expect("cancelled response");
    let reply = assert_stdio_line_accounting(&line);
    assert_eq!(reply["id"], 7);
    assert!(reply.get("result").is_none());
    assert_eq!(reply["error"]["code"], -32800);
    assert!(
        reply["error"]["data"]["reason"]
            .as_str()
            .expect("reason")
            .contains("explicitly")
    );
    writeln!(input, "{}", json!({"jsonrpc":"2.0","id":7,"method":"ping"}))
        .expect("reuse completed id");
    line.clear();
    output.read_line(&mut line).expect("fresh reply");
    let fresh = assert_stdio_line_accounting(&line);
    let budget = &fresh["result"]["_meta"]["backend/wireBudget"];
    assert_eq!(budget["bytes"], line.len(), "complete correlated ping line");
    assert_eq!(budget["tokenPolicy"], "cl100k_base/ordinary-v1");
    assert_eq!(
        budget["tokenScope"],
        "complete_jsonrpc_json_excluding_transport_framing"
    );
    assert_eq!(budget["limitTokens"], MCP_CL100K_TOKEN_LIMIT);
    assert!(wire_tokens(&fresh).expect("whole-envelope tokens") <= MCP_CL100K_TOKEN_LIMIT);
    assert_eq!(fresh["id"], 7);
    assert!(fresh.get("error").is_none());
    let result = fresh["result"].as_object().expect("fresh ping result");
    assert_eq!(result.len(), 1);
    assert!(result.contains_key("_meta"), "ping has no semantic payload");
    input
        .shutdown(std::net::Shutdown::Write)
        .expect("client shutdown");
    serving
        .join()
        .expect("actor join")
        .expect("clean stdio shutdown");
}

#[cfg(unix)]
#[test]
fn stdio_finite_pipe_half_close_drains_owner_reads_and_flushes_replies() {
    use std::io::{Read as _, Write as _};
    struct ReplyGate {
        output: std::os::unix::net::UnixStream,
        release: Option<std::sync::mpsc::Receiver<()>>,
    }
    impl std::io::Write for ReplyGate {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if let Some(release) = self.release.take() {
                release
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .map_err(|error| std::io::Error::other(error.to_string()))?;
            }
            self.output.write(bytes)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.output.flush()
        }
    }
    let (mut input, server_input) =
        std::os::unix::net::UnixStream::pair().expect("native input pipe");
    let (mut output, server_output) =
        std::os::unix::net::UnixStream::pair().expect("native response pipe");
    output
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .expect("fixture response bound");
    let (release, wait) = std::sync::mpsc::channel();
    let server = Server::with_authority(Fake::default(), PROJECT.to_owned(), [7; 32]);
    let serving = std::thread::spawn(move || {
        let mut writer = ReplyGate {
            output: server_output,
            release: Some(wait),
        };
        serve_stdio_server(server, std::io::BufReader::new(server_input), &mut writer)
    });
    for request in [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"finite-pipe","version":"1"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"backend.search","arguments":{"query":"ferris"}}}),
    ] {
        writeln!(input, "{request}").expect("finite request input");
    }
    input
        .shutdown(std::net::Shutdown::Write)
        .expect("EOF before any response can be written");
    release.send(()).expect("response may now flush");
    let mut bytes = String::new();
    output
        .read_to_string(&mut bytes)
        .expect("drain all complete replies after stdin half-close");
    serving
        .join()
        .expect("actor join")
        .expect("finite pipe success");
    let replies = bytes
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("reply JSON"))
        .collect::<Vec<_>>();
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[1]["id"], 7);
    assert_eq!(replies[1]["result"]["isError"], false);
    assert!(
        replies[1].get("error").is_none(),
        "EOF never becomes cancellation authority"
    );
}

#[test]
fn cancelled_endpoint_admission_preserves_previous_and_parked_continuation_proofs() {
    use backend_library::{
        AuthorityScopeClaim, CoverageCapability, ProducerObservationClaims,
        ProducerObservationVerifier, ScopeRoot, UntrustedProducerObservation, admit_complete_scope,
        admit_producer_observation,
    };
    use reconnect::Endpoint as _;

    struct Verifier;
    impl ProducerObservationVerifier for Verifier {
        type Error = &'static str;
        fn verify(
            &self,
            observation: &UntrustedProducerObservation,
        ) -> Result<ProducerObservationClaims, Self::Error> {
            if observation.producer_identity() != [11; 32]
                || observation.context() != [7; 32]
                || observation.evidence() != [7; 32]
            {
                return Err("foreign controlled owner");
            }
            Ok(ProducerObservationClaims::new(
                observation.producer_identity(),
                observation.scope_root(),
                observation.context(),
                *blake3::hash(observation.evidence()).as_bytes(),
            ))
        }
    }
    // This is an owned producer fixture, not insertion into Session's private
    // continuation map. The page exercises the normal certificate retention.
    let object = object_version(b"library-source-v1");
    let observation = admit_producer_observation(
        UntrustedProducerObservation::new(
            [11; 32],
            ScopeRoot::from_bytes(object.to_bytes()),
            [7; 32],
            vec![7; 32],
        ),
        &Verifier,
    )
    .expect("controlled producer observation");
    let capability = CoverageCapability::from_authorized_with_evidence(
        admit_complete_scope(
            AuthorityScopeClaim::from_object_version(object),
            observation,
        )
        .expect("controlled complete scope"),
        vec![7; 32],
    )
    .expect("controlled owner capability");
    let basis = Basis::new(view_state_root(&[]), object);
    let root = ViewRoot::new_checked(
        view_key(b"library-view-v1"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
        vec![
            Row::new(
                RowId::Symbol(symbol_key("owner::Thing")),
                basis,
                "owner::Thing",
            ),
            Row::new(
                RowId::Symbol(symbol_key("owner::Other")),
                basis,
                "owner::Other",
            ),
        ],
        vec![Coverage::Complete],
        capability,
    )
    .expect("controlled owner view");
    let owner = backend_library::Library::from_view(
        root.clone(),
        backend_library::Cursor::for_view_root(&root),
    )
    .expect("controlled owner revision");
    struct Owner(backend_library::Library);
    impl backend_client::CommandTransport for Owner {
        fn request(
            &mut self,
            request: backend_library::CommandDto,
        ) -> Result<ReplyDto, ClientError> {
            if let backend_library::Command::GraphQuery(query) = &request.command {
                let next = query
                    .next_continuation(self.0.cursor(), 1)
                    .map_err(|error| ClientError::Protocol(error.to_string()))?;
                let row =
                    backend_library::GraphQueryRow::new(std::collections::BTreeMap::from([(
                        "coordinate".to_owned(),
                        backend_library::GraphValue::String("owner::Thing".to_owned()),
                    )]))
                    .expect("owned projected row");
                return Ok(ReplyDto::new(
                    request.request_id,
                    CommandReply::GraphQueryPage(GraphQueryPage {
                        revision: query.page().basis(),
                        source: self.0.view().basis().object,
                        rows: Box::new([row]),
                        terminal: PageTerminal::More(next),
                    }),
                )
                .with_certificate(request.certificate().expect("owner request proof").clone()));
            }
            Ok(self.0.execute_dto(request))
        }
    }
    let input = AdmittedGraphQueryInput::new(
        "{ Declaration { coordinate @output } }",
        std::collections::BTreeMap::new(),
    )
    .expect("admitted query");
    let fixture = LazyAuthorityFixture::new();
    for already_parked in [false, true] {
        for expired in [false, true] {
            let mut previous =
                Session::from_transport("/owned/previous.sock", Owner(owner.clone()));
            let page = previous
                .graph_query_admitted(input.clone(), 1, None, false)
                .expect("producer-admitted continued page");
            let PageTerminal::More(next) = page.terminal else {
                panic!("continued page");
            };
            let portable = previous
                .encode_query_continuation(next)
                .expect("original proof");
            let legacy = previous.encode_page_continuation(next);
            let context = RequestContext::new();
            let expected = if expired {
                context
                    .read_deadline(std::time::Duration::ZERO)
                    .expect_err("deterministic deadline exhaustion");
                ClientError::RemoteDeadlineExceeded
            } else {
                context.cancel(backend_client::RequestCancellationReason::Explicit);
                ClientError::RequestCancelled(backend_client::RequestCancellationReason::Explicit)
            };
            let mut endpoint = SessionEndpoint {
                paths: fixture.paths.clone(),
                pending_continuation_state: None,
                request_context: Some(context),
                cli_context: None,
            };
            let previous = SessionProduct::new(previous, fixture.paths.clone());
            let previous = if already_parked {
                endpoint.retire(previous);
                None
            } else {
                Some(previous)
            };
            assert!(matches!(endpoint.connect(previous), Err(error) if error == expected));
            assert!(
                !fixture.paths.data().exists(),
                "cancelled admission cannot launch or create owner state"
            );
            let parked = endpoint
                .pending_continuation_state
                .take()
                .expect("original authority remains parked");
            let mut resumed =
                Session::from_transport("/owned/replacement.sock", Owner(owner.clone()));
            resumed.restore_continuation_state(parked);
            assert_eq!(
                resumed
                    .encode_query_continuation(next)
                    .expect("restored proof"),
                portable
            );
            assert_eq!(
                resumed
                    .decode_page_continuation(&legacy)
                    .expect("same issuing owner"),
                next
            );
        }
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::panic,
    reason = "conditional MCP projection controls"
)]
mod conditional_progress_tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn real_stdio_intake_cancels_conditional_wait_and_completed_id_can_be_reused() {
        use std::io::{BufRead as _, Write as _};
        use std::time::{Duration, Instant};
        let (mut input, server_input) =
            std::os::unix::net::UnixStream::pair().expect("owned input");
        let (output, mut server_output) =
            std::os::unix::net::UnixStream::pair().expect("owned output");
        server_input
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("fixture bound");
        output
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("response bound");
        let mut output = std::io::BufReader::new(output);
        let (admitted, admission) = std::sync::mpsc::sync_channel(1);
        let server = Server::with_authority(
            Fake {
                surface_reply: Some(phases().remove(1)),
                surface_admitted: Some(admitted),
                ..Fake::default()
            },
            PROJECT.to_owned(),
            [7; 32],
        );
        let serving = std::thread::spawn(move || {
            serve_stdio_server(
                server,
                std::io::BufReader::new(server_input),
                &mut server_output,
            )
        });
        writeln!(input, "{}", json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"conditional-progress-law","version":"1"}}})).expect("initialize");
        let mut line = String::new();
        output.read_line(&mut line).expect("initialize reply");
        assert!(assert_stdio_line_accounting(&line)["result"]["serverInfo"].is_object());
        writeln!(
            input,
            "{}",
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .expect("ack");
        let mut arguments = active_arguments();
        arguments["wait_millis"] = json!(10000);
        writeln!(input, "{}", json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":INDEX_PROGRESS_TOOL,"arguments":arguments}})).expect("conditional progress");
        admission
            .recv_timeout(Duration::from_secs(1))
            .expect("owner read occurs before cancellation");
        let started = Instant::now();
        writeln!(
            input,
            "{}",
            json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":7}})
        )
        .expect("independent intake cancellation");
        line.clear();
        output.read_line(&mut line).expect("cancelled response");
        let cancelled = assert_stdio_line_accounting(&line);
        assert_eq!(cancelled["id"], 7);
        assert_eq!(cancelled["error"]["code"], -32800);
        assert!(
            cancelled.get("result").is_none(),
            "no success/pending reference after cancellation"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "intake interrupts the actual 10-second actor wait"
        );
        writeln!(input, "{}", json!({"jsonrpc":"2.0","id":7,"method":"ping"}))
            .expect("completed id reuse");
        line.clear();
        output.read_line(&mut line).expect("next request");
        let reused = assert_stdio_line_accounting(&line);
        assert_eq!(reused["id"], 7);
        assert!(reused.get("error").is_none());
        input.shutdown(std::net::Shutdown::Write).expect("EOF");
        serving.join().expect("actor stops").expect("clean stdio");
    }

    fn phases() -> Vec<SurfaceReply> {
        let capture: Value = serde_json::from_str(include_str!(
            "../../../../crates/client/src/fixtures/go05-progress-waveform.json"
        ))
        .expect("actual waveform");
        capture["runs"]
            .as_array()
            .expect("complete states")
            .iter()
            .map(|run| serde_json::from_value(run["reply"].clone()).expect("actual receipt"))
            .collect()
    }
    #[test]
    fn operation_progress_exposes_real_profile_events_and_resumes_by_durable_key() {
        use backend_library::{
            IndexJobProgressEvent, IndexJobProgressKind, IndexOperationObservation,
            IndexOperationState, IndexProgressPage, SemanticLanguageProfile,
        };
        let active = phases().remove(1);
        let SurfaceReply::IndexOperationStatus(IndexOperationObservation::Known(status)) = &active
        else {
            panic!("active status")
        };
        let IndexOperationState::Active { ticket, stage } = &status.state else {
            panic!("active ticket")
        };
        let page = IndexProgressPage {
            ticket: ticket.clone(),
            stage: *stage,
            events: vec![IndexJobProgressEvent {
                ticket: ticket.clone(),
                sequence: 3,
                kind: IndexJobProgressKind::ProfileStarted {
                    profile: SemanticLanguageProfile::from_name("typescript")
                        .expect("native profile"),
                    ordinal: 2,
                    total: 3,
                },
            }]
            .into_boxed_slice(),
            next_sequence: 3,
            truncated: false,
            has_more: false,
        };
        for detail in ["summary", "standard", "full"] {
            let mut server = ready(Fake {
                surface_reply: Some(active.clone()),
                profile_progress_reply: Some(page.clone()),
                ..Fake::default()
            });
            let full = call(
                &mut server,
                INDEX_PROGRESS_TOOL,
                &json!({"operation_key":status.operation_key,"detail":detail}),
            );
            assert_eq!(full["isError"], false);
            assert!(text_of(&full).contains("profile 2/3"));
            assert_eq!(
                full["structuredContent"]["surface"]["index_job"],
                serde_json::to_value(backend_present::IndexJobProjection::Progress(
                    backend_library::IndexJobObservation::Pending(page.clone())
                ))
                .expect("existing progress DTO")
            );
            let action = full["structuredContent"]["index_poll"]["call"].clone();
            assert_eq!(
                action["arguments"]["operation_key"],
                json!(status.operation_key)
            );
            assert!(action["arguments"].get("ticket").is_none());
            server.product.surface_reply = Some(active.clone());
            let mut arguments = action["arguments"].clone();
            arguments["wait_millis"] = json!(0);
            let unchanged = call(&mut server, INDEX_PROGRESS_TOOL, &arguments);
            assert_eq!(
                unchanged["structuredContent"]["surface"]["data"]["state"],
                "pending-unchanged"
            );
            server.product.surface_reply = Some(active.clone());
            let mut advanced = page.clone();
            advanced.next_sequence = 4;
            advanced.events[0].sequence = 4;
            advanced.events[0].kind = IndexJobProgressKind::ProfileAdmitted {
                profile: SemanticLanguageProfile::from_name("typescript").expect("profile"),
                ordinal: 2,
                total: 3,
            };
            server.product.profile_progress_reply = Some(advanced);
            let changed = call(&mut server, INDEX_PROGRESS_TOOL, &arguments);
            assert_eq!(changed["isError"], false);
            assert!(text_of(&changed).contains("staged; publication pending"));
            assert_ne!(
                changed["structuredContent"]["index_poll"]["call"]["arguments"]["after_observation"],
                arguments["after_observation"]
            );
            assert_eq!(durable_commands(&server.product).len(), 3);
            assert_eq!(server.product.surface_commands.iter().filter(|command| matches!(command, SurfaceCommand::IndexProgress { ticket: actual, .. } if actual == ticket)).count(), 3);
            assert_eq!(server.product.probe_calls, 0);
            assert_context_bounded(&full);
            assert_context_bounded(&unchanged);
            assert_context_bounded(&changed);
        }
    }

    #[test]
    fn escaped_profile_refusals_page_losslessly_within_real_mcp_budget_and_following_read() {
        use backend_library::interface::{
            CompilerAttempt, CompilerFragmentFailure, SourceAuthority,
        };
        use backend_library::{
            IndexJobObservation, IndexJobProgressEvent, IndexJobProgressKind,
            IndexOperationObservation, IndexOperationState, IndexProgressPage,
            PackageCompilerFailure, SemanticLanguageProfile,
        };
        use backend_semantic::ir::{BuildError, EntityId};
        let mut active = phases().remove(1);
        let SurfaceReply::IndexOperationStatus(IndexOperationObservation::Known(status)) =
            &mut active
        else {
            panic!("active status");
        };
        let IndexOperationState::Active { ticket, stage } = &status.state else {
            panic!("active ticket");
        };
        let ticket = ticket.clone();
        let stage = *stage;
        let operation_key = status.operation_key;
        let capture = status
            .source_capture
            .as_mut()
            .expect("actual source capture shape");
        let prototype = capture.profiles[0];
        let mut profiles = SemanticLanguageProfile::names()
            .into_iter()
            .map(|name| backend_library::IndexOperationSourceProfile {
                profile: SemanticLanguageProfile::from_name(name).expect("closed profile"),
                ..prototype
            })
            .collect::<Vec<_>>();
        profiles.sort_by_key(|profile| profile.profile);
        capture.profiles = profiles.clone().into_boxed_slice();
        active
            .admit(backend_library::CommandId::IndexProgress)
            .expect("all nine legitimate captured profiles");
        let mut events = Vec::new();
        for (position, source) in profiles.iter().take(8).enumerate() {
            let ordinal = u16::try_from(position + 1).expect("nine-profile header");
            // Quotes are permitted path bytes and exercise actual JSON escaping.
            let path = format!(
                "{}{}",
                "\"".repeat(PackageCompilerFailure::MAX_RELATIVE_PATH_BYTES - 1),
                position
            );
            let failure = PackageCompilerFailure::from_fragment_failure(
                &path,
                CompilerAttempt {
                    source: SourceAuthority {
                        identity: backend_version::ContentId::<
                            backend_version::SourceFactDomain,
                        >::from_canonical_bytes(b"source bytes"),
                        byte_len: 12,
                    },
                    recipe: backend_version::ContentId::<
                        backend_version::CompileRecipeDomain,
                    >::from_canonical_bytes(b"recipe bytes"),
                },
                &CompilerFragmentFailure::build(BuildError::InvalidOccurrenceSpan {
                    owner: EntityId::new(7),
                    start: 18,
                    end: 24,
                }),
            )
            .expect("maximal escaped canonical path")
            .public_summary();
            let encoded = failure.encode_bounded_json().expect("bounded exact cause");
            assert!(
                encoded.len() > 6 * 1024
                    && encoded.len() <= PackageCompilerFailure::MAX_ENCODED_BYTES
            );
            for kind in [
                IndexJobProgressKind::ProfileStarted {
                    profile: source.profile,
                    ordinal,
                    total: 9,
                },
                IndexJobProgressKind::ProfileRefused {
                    profile: source.profile,
                    ordinal,
                    total: 9,
                    failure: Box::new(failure),
                },
            ] {
                events.push(IndexJobProgressEvent {
                    ticket: ticket.clone(),
                    sequence: u64::try_from(events.len() + 1).expect("bounded history"),
                    kind,
                });
            }
        }
        let page = IndexProgressPage {
            ticket: ticket.clone(),
            stage,
            events: events.clone().into_boxed_slice(),
            next_sequence: 16,
            truncated: false,
            has_more: false,
        };
        SurfaceReply::IndexProgress(IndexJobObservation::Pending(page.clone()))
            .admit(backend_library::CommandId::IndexProgress)
            .expect("original exact owner page");
        assert!(
            serde_json::to_vec(&page).expect("full owner page").len()
                > DEFAULT_RESPONSE_BUDGET_BYTES
        );
        let original = active.clone();
        let mut server = ready(Fake {
            surface_reply: Some(active),
            profile_progress_reply: Some(page.clone()),
            ..Fake::default()
        });
        let mut arguments = json!({"operation_key":operation_key,"detail":"full"});
        let mut seen = Vec::new();
        let mut windows = 0;
        loop {
            windows += 1;
            assert!(windows <= events.len(), "every exact cursor must advance");
            let response = request(
                &mut server,
                "tools/call",
                &json!({"name":INDEX_PROGRESS_TOOL,"arguments":arguments}),
            );
            assert!(
                response.get("error").is_none(),
                "real stdio envelope must not replace the page with an oversized fault: {response}"
            );
            assert_eq!(response["result"]["isError"], false);
            assert_context_bounded(&response);
            let tokens =
                cl100k_tokens(&serde_json::to_string(&response).expect("actual full response"))
                    .expect("real token policy");
            assert!(tokens <= MCP_CL100K_TOKEN_LIMIT);
            let body = &response["result"]["structuredContent"];
            if windows == 1 {
                let SurfaceReply::IndexOperationStatus(observation) = &original else {
                    panic!("status");
                };
                assert_eq!(
                    body["surface"]["data"],
                    serde_json::to_value(observation).expect("same durable receipt")
                );
            }
            let projected: backend_present::IndexJobProjection =
                serde_json::from_value(body["surface"]["index_job"].clone())
                    .expect("exact typed page");
            let backend_present::IndexJobProjection::Progress(IndexJobObservation::Pending(window)) =
                projected
            else {
                panic!("pending window");
            };
            assert_eq!(window.ticket, ticket);
            assert!(!window.events.is_empty());
            let prior = seen.len();
            assert_eq!(
                &events[prior..prior + window.events.len()],
                window.events.as_ref(),
                "no cause/event may be changed, skipped or repeated"
            );
            seen.extend(window.events.iter().cloned());
            assert_eq!(
                window.next_sequence,
                u64::try_from(seen.len()).expect("sequence")
            );
            if !window.has_more {
                break;
            }
            let call = &body["index_poll"]["call"];
            assert_eq!(call["name"], INDEX_PROGRESS_TOOL);
            arguments = call["arguments"].clone();
            assert_eq!(
                arguments["ticket"],
                serde_json::to_value(&ticket).expect("ticket")
            );
            assert_eq!(arguments["after_sequence"], window.next_sequence);
            assert!(
                arguments.get("wait_millis").is_none(),
                "remaining history is immediately readable"
            );
            let remainder = IndexProgressPage {
                events: events[seen.len()..].to_vec().into_boxed_slice(),
                ..page.clone()
            };
            server.product.surface_reply = Some(SurfaceReply::IndexProgress(
                IndexJobObservation::Pending(remainder),
            ));
        }
        assert!(windows > 1, "the overflowing owner page must be continued");
        assert_eq!(
            seen, events,
            "all eight exact typed refusals survive the full cursor chain"
        );
        // Even the final single maximal-path refusal must remain intact with
        // the complete nine-profile durable status, not become an oversized
        // fault after the window can no longer shrink.
        let one = IndexProgressPage {
            events: vec![events[1].clone()].into_boxed_slice(),
            next_sequence: events[1].sequence,
            ..page.clone()
        };
        server.product.surface_reply = Some(original.clone());
        server.product.profile_progress_reply = Some(one.clone());
        let single = request(
            &mut server,
            "tools/call",
            &json!({"name":INDEX_PROGRESS_TOOL,"arguments":{"operation_key":operation_key,"detail":"full"}}),
        );
        assert!(
            single.get("error").is_none(),
            "one valid maximum-path cause must fit or remain an explicit bounded refusal"
        );
        assert_eq!(single["result"]["isError"], false);
        assert_context_bounded(&single);
        let projection: backend_present::IndexJobProjection = serde_json::from_value(
            single["result"]["structuredContent"]["surface"]["index_job"].clone(),
        )
        .expect("one exact cause");
        assert_eq!(
            projection,
            backend_present::IndexJobProjection::Progress(IndexJobObservation::Pending(one))
        );
        let status = call(&mut server, "backend.status", &json!({}));
        assert_eq!(status["isError"], false);
        assert_eq!(status["structuredContent"]["answer"], "status");
        assert!(
            text_of(&status).contains("oracles 0/"),
            "following read remains useful in this same initialized session"
        );
        assert!(
            server
                .product
                .surface_commands
                .iter()
                .all(|command| matches!(
                    command,
                    SurfaceCommand::IndexProgress { .. }
                        | SurfaceCommand::IndexOperationStatus { .. }
                )),
            "projection must never dispatch, cancel, replace or re-admit work"
        );
    }

    fn durable_commands(product: &Fake) -> Vec<SurfaceCommand> {
        product
            .surface_commands
            .iter()
            .filter(|command| !matches!(command, SurfaceCommand::IndexProgress { .. }))
            .cloned()
            .collect()
    }

    fn active_arguments() -> Value {
        let replies = phases();
        let SurfaceReply::IndexOperationStatus(backend_library::IndexOperationObservation::Known(
            status,
        )) = &replies[1]
        else {
            panic!("actual active")
        };
        json!({"operation_key":status.operation_key, "after_observation":backend_client::IndexObservationFingerprint::active(status).expect("actual full fingerprint").expect("active"), "wait_millis":0})
    }

    #[test]
    fn conditional_mcp_progress_is_explicit_pending_and_old_full_snapshot_remains_exact() {
        for detail in ["summary", "standard", "full"] {
            let active = phases().remove(1);
            let mut server = ready(Fake {
                surface_reply: Some(active.clone()),
                ..Fake::default()
            });
            let mut arguments = active_arguments();
            arguments["detail"] = json!(detail);
            let unchanged = call(&mut server, INDEX_PROGRESS_TOOL, &arguments);
            assert_eq!(unchanged["isError"], false);
            let body = &unchanged["structuredContent"];
            assert_eq!(body["surface"]["data"]["state"], "pending-unchanged");
            let reference = &body["surface"]["data"]["reference"];
            assert_eq!(reference["operation_key"], arguments["operation_key"]);
            assert_eq!(
                reference["after_observation"],
                arguments["after_observation"]
            );
            assert_eq!(
                reference["ticket"],
                serde_json::to_value(&active).expect("exact status")["data"]["detail"]["state"]["detail"]
                    ["ticket"]
            );
            assert_eq!(
                body["index_poll"]["call"]["arguments"]["wait_millis"],
                10000
            );
            assert_eq!(body["index_poll"]["call"]["arguments"]["detail"], detail);
            assert!(text_of(&unchanged).contains("remains pending"));
            assert!(!text_of(&unchanged).contains("published"));
            assert!(
                serde_json::to_vec(&unchanged)
                    .expect("complete result")
                    .len()
                    < 2200
            );
            assert_context_bounded(&unchanged);
            server.product.surface_reply = Some(active.clone());
            let complete = call(
                &mut server,
                INDEX_PROGRESS_TOOL,
                &json!({"operation_key":arguments["operation_key"],"detail":"full"}),
            );
            assert_eq!(
                complete["structuredContent"]["surface"],
                serde_json::to_value(&active).expect("old full request is exact")
            );
            assert_eq!(
                complete["structuredContent"]["index_poll"]["call"]["arguments"],
                json!({"operation_key":arguments["operation_key"], "after_observation":arguments["after_observation"], "wait_millis":10000, "detail":"full"})
            );
            assert_eq!(
                durable_commands(&server.product).len(),
                2,
                "legacy request remains immediate"
            );
            assert!(
                complete["structuredContent"]
                    .get("index_progress")
                    .is_none()
            );
        }
    }

    #[test]
    fn copied_pending_action_keeps_full_receipt_detail_and_exact_operation_until_failure() {
        for detail in ["summary", "standard", "full"] {
            let active = phases().remove(1);
            let terminal = phases().pop().expect("actual refusal");
            let mut arguments = active_arguments();
            arguments["detail"] = json!(detail);
            let mut server = ready(Fake {
                surface_replies: [active, terminal.clone()].into(),
                ..Fake::default()
            });
            let pending = call(&mut server, INDEX_PROGRESS_TOOL, &arguments);
            let next = &pending["structuredContent"]["index_poll"]["call"];
            assert_eq!(next["arguments"]["detail"], detail);
            assert_eq!(
                next["arguments"]["operation_key"],
                arguments["operation_key"]
            );
            assert_eq!(
                next["arguments"]["after_observation"],
                arguments["after_observation"]
            );
            assert!(text_of(&pending).contains(&next.to_string()));
            let failed = call(
                &mut server,
                next["name"].as_str().expect("same tool"),
                &next["arguments"],
            );
            assert_eq!(failed["isError"], true);
            assert_eq!(failed["structuredContent"]["detail"], detail);
            assert_eq!(
                failed["structuredContent"]["surface"],
                serde_json::to_value(&terminal).expect("entire failed receipt")
            );
            assert!(failed["structuredContent"].get("index_poll").is_none());
            assert_eq!(durable_commands(&server.product).len(), 2);
            assert_eq!(
                durable_commands(&server.product)[0],
                durable_commands(&server.product)[1]
            );
            assert_eq!(
                server.product.probe_calls, 0,
                "no mutation or preparation retry"
            );
            assert_context_bounded(&pending);
            assert_context_bounded(&failed);
        }
    }

    #[test]
    fn supplied_fingerprint_defaults_to_wait_but_explicit_zero_stays_immediate() {
        for explicit_zero in [false, true] {
            let active = phases().remove(1);
            let terminal = phases().pop().expect("actual refusal");
            let mut arguments = active_arguments();
            if !explicit_zero {
                arguments
                    .as_object_mut()
                    .expect("arguments")
                    .remove("wait_millis");
            }
            let mut server = ready(Fake {
                surface_replies: [active, terminal.clone()].into(),
                ..Fake::default()
            });
            let observed = call(&mut server, INDEX_PROGRESS_TOOL, &arguments);
            if explicit_zero {
                assert_eq!(
                    observed["structuredContent"]["surface"]["data"]["state"],
                    "pending-unchanged"
                );
                assert_eq!(durable_commands(&server.product).len(), 1);
                assert_eq!(
                    server.product.surface_replies.len(),
                    1,
                    "next state was not probed"
                );
            } else {
                assert_eq!(
                    observed["structuredContent"]["surface"],
                    serde_json::to_value(&terminal).expect("changed failure")
                );
                assert_eq!(durable_commands(&server.product).len(), 2);
            }
        }
    }

    #[test]
    #[cfg(any(unix, windows))]
    fn conditional_progress_cannot_replace_original_deadline_or_cancelled_context() {
        for cancelled in [false, true] {
            let context = RequestContext::new();
            if cancelled {
                context.cancel(backend_client::RequestCancellationReason::Explicit);
            } else {
                assert_eq!(
                    context.read_deadline(std::time::Duration::ZERO),
                    Err(ClientError::RemoteDeadlineExceeded)
                );
            }
            let mut server = ready(Fake::default());
            let mut arguments = active_arguments();
            arguments
                .as_object_mut()
                .expect("arguments")
                .remove("wait_millis");
            let wire = server.handle_value(
                json!({"jsonrpc":"2.0", "id":41, "method":"tools/call", "params":{"name":INDEX_PROGRESS_TOOL,"arguments":arguments}}),
                Some(context.clone()),
            ).expect("typed request refusal");
            assert_eq!(wire["id"], 41);
            if cancelled {
                assert!(wire.get("result").is_none());
                assert_eq!(wire["error"]["code"], -32800);
            } else {
                assert_eq!(wire["result"]["isError"], true);
                assert_eq!(wire["result"]["structuredContent"]["slug"], "endpoint");
                assert_ne!(wire["result"]["structuredContent"]["answer"], "surface");
                assert_eq!(context.check(), Err(ClientError::RemoteDeadlineExceeded));
                assert_eq!(
                    context.read_deadline(std::time::Duration::from_secs(30)),
                    Err(ClientError::RemoteDeadlineExceeded),
                    "re-admission must not install a fresh read budget"
                );
            }
            assert!(
                durable_commands(&server.product).is_empty(),
                "stopped before owner read"
            );
        }
    }

    #[test]
    fn conditional_mcp_terminal_refusal_keeps_every_actual_go05_fault_and_source_fact() {
        let terminal = phases().pop().expect("actual refusal");
        for detail in ["summary", "standard", "full"] {
            let mut server = ready(Fake {
                surface_reply: Some(terminal.clone()),
                ..Fake::default()
            });
            let mut arguments = active_arguments();
            arguments["wait_millis"] = json!(10000);
            arguments["detail"] = json!(detail);
            let result = call(&mut server, INDEX_PROGRESS_TOOL, &arguments);
            assert_eq!(result["isError"], true);
            let actual = serde_json::to_value(&terminal).expect("actual typed terminal");
            assert_eq!(result["structuredContent"]["surface"], actual);
            assert_eq!(
                result["structuredContent"]["fault"]["compiler_failure"],
                actual["data"]["detail"]["state"]["detail"]["compiler_failure"]
            );
            assert!(result["structuredContent"].get("index_poll").is_none());
            assert_eq!(
                durable_commands(&server.product).len(),
                1,
                "terminal returns immediately despite requested wait"
            );
            assert_context_bounded(&result);
        }
    }

    #[test]
    fn conditional_mcp_uses_existing_ticket_sequence_and_returns_terminal_without_new_event() {
        let ticket = super::index_job_ticket();
        let pending = SurfaceReply::IndexProgress(backend_library::IndexJobObservation::Pending(
            backend_library::IndexProgressPage {
                ticket: ticket.clone(),
                stage: backend_library::IndexJobStage::Compiling,
                events: Box::new([]),
                next_sequence: 7,
                truncated: false,
                has_more: false,
            },
        ));
        let terminal = SurfaceReply::IndexProgress(backend_library::IndexJobObservation::Terminal(
            backend_library::IndexJobTerminal {
                ticket: ticket.clone(),
                outcome: backend_library::IndexJobOutcome::Cancelled,
            },
        ));
        let mut server = ready(Fake {
            surface_replies: [pending, terminal.clone()].into(),
            ..Fake::default()
        });
        let result = call(
            &mut server,
            INDEX_PROGRESS_TOOL,
            &json!({"ticket":ticket,"after_sequence":7,"wait_millis":1}),
        );
        assert_eq!(result["isError"], false);
        assert_eq!(
            result["structuredContent"]["surface"]["data"],
            serde_json::to_value(&terminal).expect("terminal")["data"]
        );
        assert_eq!(server.product.surface_commands.len(), 2);
        assert!(
            server
                .product
                .surface_commands
                .iter()
                .all(|command| matches!(
                    command,
                    SurfaceCommand::IndexProgress {
                        after_sequence: 7,
                        ..
                    }
                ))
        );
    }

    #[test]
    fn conditional_mcp_input_refusals_make_no_owner_calls() {
        let valid = active_arguments();
        let key = valid["operation_key"].clone();
        for input in [
            json!({"operation_key":key,"wait_millis":10000}),
            json!({"operation_key":key,"after_observation":valid["after_observation"],"wait_millis":10001}),
            json!({"operation_key":key,"after_observation":"AA".repeat(32)}),
            json!({"operation_key":key,"wait_millis":-1}),
            json!({"ticket":super::index_job_ticket(),"after_observation":valid["after_observation"]}),
        ] {
            let mut server = ready(Fake::default());
            let result = call(&mut server, INDEX_PROGRESS_TOOL, &input);
            assert_eq!(result["isError"], true);
            assert!(server.product.surface_commands.is_empty());
        }
    }
}

#[test]
fn all_mcp_record_details_preserve_useful_text_and_complete_structured_identity() {
    for tool in [
        "backend.search",
        "backend.resolve",
        "backend.graph",
        "backend.related",
    ] {
        for detail in ["summary", "standard", "full"] {
            let mut server = ready(Fake::default());
            let arguments = if matches!(tool, "backend.search" | "backend.resolve") {
                json!({"query":"ferris", "limit":1, "detail":detail})
            } else {
                json!({"coordinate":DECLARATION, "detail":detail})
            };
            let response = request(
                &mut server,
                "tools/call",
                &json!({"name":tool,"arguments":arguments}),
            );
            assert_context_bounded(&response);
            let result = &response["result"];
            assert_eq!(result["isError"], false, "{tool}: {response}");
            let typed = &result["structuredContent"];
            assert_eq!(typed["detail"], detail);
            let records = typed["records"].as_array().expect("complete rows");
            let preview = text_of(result);
            let coordinates = summary_coordinates(&preview);
            assert!(!coordinates.is_empty(), "{tool}/{detail}: {preview}");
            assert_eq!(
                coordinates,
                records
                    .iter()
                    .map(|row| row["identity"]["coordinate"]
                        .as_str()
                        .expect("exact operand")
                        .to_owned())
                    .collect::<Vec<_>>()
            );
            let decoded: backend_present::RecordListDto =
                serde_json::from_value(typed.clone()).expect("stable typed row projection");
            assert_eq!(decoded.records.len(), records.len());
            assert!(preview.contains(&format!(
                "showing {} exact coordinate(s); 0 omitted",
                coordinates.len()
            )));
        }
    }
}

#[test]
fn final_record_preview_shrink_preserves_page_footer_across_ids_and_transports() {
    let (mut text, coordinates) = summary_rows_fixture(100);
    text.push_str("Continue `backend.graph` with the same arguments; copy `structuredContent.nextCursor` to `arguments.cursor`.\n");
    let structured = json!({"records": " ".repeat(32_000), "nextCursor": "exact-owner-token"});
    for transport in [ResponseTransport::StdioLine, ResponseTransport::HttpBody] {
        for id in [
            json!("אב\"\\🙂".repeat(1000)),
            json!("request-".to_owned() + &"r".repeat(8_000)),
        ] {
            let response = bound_rpc_reply(
                success(id.clone(), tool_result(&text, structured.clone(), false)),
                transport,
            );
            assert!(wire_fits(&response, transport), "{response}");
            assert_eq!(response["id"], id);
            assert_eq!(response["result"]["structuredContent"], structured);
            let preview = text_of(&response["result"]);
            let shown = summary_coordinates(&preview);
            assert_eq!(shown, coordinates[..shown.len()]);
            assert!(preview.contains(&format!(
                "showing {} exact coordinate(s); {} omitted",
                shown.len(),
                100 - shown.len()
            )));
            assert!(preview.contains("Continue `backend.graph` with the same arguments"));
            assert!(!preview.contains("exact-owner-token"));
            assert!(wire_tokens(&response).expect("named tokenizer") <= MCP_CL100K_TOKEN_LIMIT);
        }
    }
}

#[test]
fn dense_record_budget_witnesses_remain_named_token_refusals() {
    let (text, _) = summary_rows_fixture(100);
    for bytes in [34_000, 41_000] {
        let structured =
            json!({"records":"s".repeat(bytes),"nextCursor":"owner-cursor-remains-structured"});
        let candidate = success(json!(71), tool_result(&text, structured, false));
        assert!(wire_tokens(&candidate).expect("reference dense witness") > MCP_CL100K_TOKEN_LIMIT);
        let response = bound_rpc_reply(candidate, ResponseTransport::StdioLine);
        assert_eq!(response["id"], 71);
        assert!(wire_fits(&response, ResponseTransport::StdioLine));
        assert!(response["error"].is_object());
        assert_eq!(
            response["error"]["data"]["structuredContent"]["cause"],
            "oversized"
        );
        assert!(
            response["error"]["data"]["detail"]
                .as_str()
                .expect("typed refusal")
                .contains(CL100K_POLICY)
        );
    }
}

#[test]
fn index_poll_cli_advice_is_optional_and_preserves_installed_attachment_key_and_detail() {
    let key = backend_library::IndexOperationKey::from_bytes([0x71; 32]).expect("key");
    let observation = backend_library::IndexOperationObservation::Known(
        backend_library::IndexOperationStatus::new(
            key,
            PackageReference::parse(PROJECT).expect("package"),
            CompileExecutionIntent::Interactive,
            backend_library::IndexOperationState::Accepted,
        ),
    );
    let context = backend_present::StartupRecoveryContext::new(
        PROJECT.to_owned(),
        "/private/owner.sock".to_owned(),
        "/observed/bundle/backend-cli".to_owned(),
        backend_present::StartupRecoveryCommand::Status,
        backend_present::StartupRecoveryMode::Active,
        backend_present::StartupRecoveryWorkspace::Explicit("/private/workspace".to_owned()),
    );
    for detail in ["summary", "standard", "full"] {
        for installed in [false, true] {
            let mut server = ready(Fake {
                surface_reply: Some(SurfaceReply::IndexOperationStatus(observation.clone())),
                cli_context: installed.then(|| context.clone()),
                ..Fake::default()
            });
            let response = request(
                &mut server,
                "tools/call",
                &json!({"name":INDEX_PROGRESS_TOOL,"arguments":{"operation_key":key,"detail":detail}}),
            );
            assert_context_bounded(&response);
            let json = serde_json::to_string(&response).expect("actual admitted JSON envelope");
            assert!(
                cl100k_tokens(&json).expect("named wire token policy") <= MCP_CL100K_TOKEN_LIMIT
            );
            assert_eq!(
                response["result"]["_meta"]["backend/wireBudget"]["bytes"],
                json.len() + 1
            );
            let result = &response["result"];
            assert_eq!(result["isError"], false, "{result}");
            let poll = &result["structuredContent"]["index_poll"];
            assert_eq!(
                poll["call"],
                json!({"name":"backend.index_progress","arguments":{"operation_key":key,"detail":detail}})
            );
            assert_eq!(poll.get("shell").is_some(), installed);
            if installed {
                let expected =
                    backend_present::Affordance::PollIndexOperation { operation_key: key }
                        .poll_shell_in_context(&context, backend_present::Detail::parse(detail));
                assert_eq!(poll["shell"], expected.expect("same admitted CLI context"));
                assert!(
                    !poll["shell"]
                        .as_str()
                        .expect("shell")
                        .contains("authority.secret")
                );
                assert!(!poll["shell"].as_str().expect("shell").contains("nudox"));
            }
            assert_eq!(
                server.product.surface_commands,
                [SurfaceCommand::IndexOperationStatus { operation_key: key }]
            );
            let catalog = tools::list_tools(&json!({})).expect("actual listed tool catalog");
            let grammar = tool_named(&catalog["tools"], INDEX_PROGRESS_TOOL);
            assert_eq!(
                grammar["outputSchema"]["properties"]["index_poll"]["required"],
                json!(["call"])
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn cli_attachment_metadata_does_not_connect_an_owner_or_display_custom_authority() {
    const PROBE: &str = "jsonrpc::tests::cli_attachment_metadata_probe";
    let executable = std::env::current_exe().expect("test binary");
    for custom in [false, true] {
        let mut child = std::process::Command::new(&executable);
        child
            .args(["--exact", PROBE, "--nocapture"])
            .env("NUDOX_TEST_CLI_CONTEXT", "1");
        if custom {
            child.env(
                backend_runtime::AUTHORITY_SECRET_ENV,
                "/not-displayed/custom-authority",
            );
        } else {
            child.env_remove(backend_runtime::AUTHORITY_SECRET_ENV);
        }
        let output = child.output().expect("read-only metadata subprocess");
        assert!(output.status.success(), "{output:?}");
        let stdout = std::str::from_utf8(&output.stdout).expect("UTF-8");
        let data = stdout
            .lines()
            .find_map(|line| line.strip_prefix("CLI_CONTEXT="))
            .expect("context observation");
        let value: Value = serde_json::from_str(data).expect("typed observation");
        assert_eq!(value["has_context"], !custom);
        assert_eq!(value["data_exists"], false);
        assert_eq!(value["endpoint_exists"], false);
        assert!(!data.contains("custom-authority"));
    }
}

#[test]
fn cli_attachment_metadata_probe() {
    if std::env::var("NUDOX_TEST_CLI_CONTEXT").is_err() {
        return;
    }
    let base = std::env::temp_dir().join(format!("cli-context-probe-{}", std::process::id()));
    let paths = backend_runtime::WorkspacePaths::discover(
        Some(base.join("project")),
        Some(base.join("state")),
        Some(base.join("owner.sock")),
    )
    .expect("uncreated attachment");
    #[cfg(unix)]
    let context = {
        use std::os::unix::fs::PermissionsExt as _;
        let bundle = base.with_extension("observed-bundle");
        std::fs::create_dir(&bundle).expect("owned installed CLI fixture");
        let cli = bundle.join("backend-cli");
        std::fs::write(&cli, b"#!/bin/sh\nexit 0\n").expect("actual executable fixture");
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700))
            .expect("CLI execute mode");
        assert!(cli_context_for_executable(&paths, &bundle.join("missing-cli")).is_none());
        let context = cli_context_for_executable(&paths, &cli);
        std::fs::remove_dir_all(bundle).expect("remove owned metadata fixture");
        context
    };
    #[cfg(not(unix))]
    let context: Option<backend_present::StartupRecoveryContext> = None;
    println!(
        "\nCLI_CONTEXT={}",
        json!({"has_context":context.is_some(),"data_exists":paths.data().exists(),"endpoint_exists":paths.endpoint().exists(),"shell":context.map(|context|context.shell())})
    );
}

#[test]
fn search_published_names_preserves_useful_records_and_explicit_pending_scope() {
    for detail in [Detail::Summary, Detail::Standard, Detail::Full] {
        let mut server = ready(Fake {
            published_name_coverage: Some(vec![Coverage::Partial {
                lane: Lane::Semantic, completed: 0, total: 1,
            }]),
            published_name_readiness: Some(backend_library::PublishedSearchReadiness {
                full_search: backend_library::FullSearchReadiness::Preparing,
                semantic: backend_library::PublishedSemanticReadiness::Pending,
            }),
            ..Fake::default()
        });
        let response = request(
            &mut server,
            "tools/call",
            &json!({
                "name": "backend.search", "arguments": {"query": "Session", "limit": 10, "detail": detail.name()}
            }),
        );
        assert_context_bounded(&response);
        assert!(response.get("error").is_none(), "{response}");
        let result = &response["result"];
        assert_eq!(result["isError"], false, "{response}");
        let page = &result["structuredContent"];
        assert_eq!(page["readiness"], "partial");
        assert_eq!(page["search_scope"], "published_names_only");
        assert_eq!(page["full_search"], "preparing");
        assert_eq!(page["semantic_preparation"]["state"], "pending");
        assert_eq!(page["records"].as_array().expect("useful rows").len(), 1);
        assert_eq!(page["records"][0]["identity"]["coordinate"], DECLARATION);
        assert_eq!(
            page["records"][0]["identity"]["semantic_data"]["value"],
            json!(*symbol_key(DECLARATION).as_bytes())
        );
        assert!(
            result["content"]
                .as_array()
                .expect("readable content")
                .iter()
                .any(|content| content["text"].as_str().is_some_and(|text| text
                    .contains("published-names-only")
                    && text.contains("pending")))
        );
    }
}

#[test]
fn search_published_names_never_masks_a_declared_compiler_refusal_as_partial() {
    let mut server = ready(Fake {
        published_name_readiness: Some(backend_library::PublishedSearchReadiness {
            full_search: backend_library::FullSearchReadiness::Preparing,
            semantic: backend_library::PublishedSemanticReadiness::Unavailable {
                reason: backend_library::SemanticSearchReason::Unconfigured,
            },
        }),
        // The existing default snapshot declares an unconfigured semantic lane.
        ..Fake::default()
    });
    let response = request(&mut server, "tools/call", &json!({
        "name": "backend.search", "arguments": {"query": "Session", "limit": 10}
    }));
    assert_context_bounded(&response);
    assert!(response.get("error").is_none(), "{response}");
    let result = &response["result"];
    assert_eq!(result["isError"], false, "{response}");
    let page = &result["structuredContent"];
    assert_eq!(page["readiness"], "unavailable");
    assert_eq!(page["search_scope"], "published_names_only");
    assert_eq!(page["full_search"], "preparing");
    assert_eq!(page["semantic_preparation"]["state"], "unavailable");
    assert_eq!(page["semantic_preparation"]["reason"], "unconfigured");
    assert_eq!(page["records"].as_array().expect("useful retained names").len(), 1);
}
