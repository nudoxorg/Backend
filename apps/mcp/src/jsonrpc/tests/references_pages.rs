//! `backend.references` pages a long answer instead of failing it.
use super::*;

fn target() -> ProductText {
    ProductText::new("pkg::semantic::callee").expect("target text")
}

fn listing(count: usize, pad: usize) -> SurfaceReply {
    let references = (0..count)
        .map(|index| backend_library::ReferenceRecord {
            site_symbol: None,
            source_policy: Default::default(),
            site: ProductText::new(format!("pkg::caller_{index}::{}", "x".repeat(pad))).expect("site"),
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
                    file: ProductText::new("src/main.rs").expect("path text"),
                    start: u32::try_from(index).expect("small"),
                    end: u32::try_from(index).expect("small") + 1,
                }),
            },
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();
    SurfaceReply::References {
        target: target(),
        coverage: backend_library::ReferenceCoverage::default(),
        references,
    }
}

/// Reads every page of one listing, feeding the owner reply to each call, and
/// returns the page positions with the span starts seen.
fn read_all(server: &mut Server<Fake>, reply: &SurfaceReply, first: Value) -> (Vec<Value>, Vec<u64>) {
    let mut arguments = first;
    let mut pages = Vec::new();
    let mut starts = Vec::new();
    loop {
        server.product.surface_replies.push_back(reply.clone());
        let result = call(server, "backend.references", &arguments);
        assert_eq!(result["isError"], false, "{result}");
        assert_context_bounded(&result);
        let structured = &result["structuredContent"];
        for record in structured["records"].as_array().expect("records") {
            starts.push(record["reference_details"]["evidence"]["source"]["start"].as_u64().expect("start"));
        }
        pages.push(structured["references_page"].clone());
        match structured.get("continuation_call") {
            Some(next) => arguments = next["arguments"].clone(),
            None => return (pages, starts),
        }
    }
}

#[test]
fn references_page_by_limit_and_cursor_until_every_use_is_read() {
    let mut server = ready(Fake::default());
    let reply = listing(7, 0);
    let (pages, starts) = read_all(
        &mut server,
        &reply,
        json!({"coordinate": "pkg::semantic::callee", "limit": 3}),
    );
    assert_eq!(starts, (0..7).collect::<Vec<u64>>(), "every use once, in order");
    let positions = pages
        .iter()
        .map(|page| (page["offset"].as_u64(), page["returned"].as_u64(), page["remaining"].as_u64()))
        .collect::<Vec<_>>();
    assert_eq!(positions, [(Some(0), Some(3), Some(4)), (Some(3), Some(3), Some(1)), (Some(6), Some(1), Some(0))]);
    assert!(pages.iter().all(|page| page["total"] == 7));
    assert_eq!(server.product.surface_commands.len(), 3);
    assert!(server
        .product
        .surface_commands
        .iter()
        .all(|command| matches!(command, SurfaceCommand::References { .. })));
}

#[test]
fn the_first_page_says_how_many_uses_remain_and_names_the_continuation() {
    let mut server = ready(Fake::default());
    server.product.surface_replies.push_back(listing(7, 0));
    let result = call(
        &mut server,
        "backend.references",
        &json!({"coordinate": "pkg::semantic::callee", "limit": 3}),
    );
    let text = text_of(&result);
    assert!(text.contains("Uses 1-3 of 7; 4 more remain"), "{text}");
    let structured = &result["structuredContent"];
    assert!(structured["nextCursor"].as_str().is_some_and(|cursor| cursor.starts_with("mcp2-")), "{structured}");
    assert_eq!(structured["continuation_call"]["name"], "backend.references");
    assert_eq!(structured["continuation_call"]["arguments"]["limit"], 3);
    assert_eq!(
        structured["continuation_call"]["arguments"]["coordinate"],
        "pkg::semantic::callee"
    );
}

#[test]
fn a_listing_larger_than_the_response_budget_degrades_to_a_partial_answer() {
    let mut server = ready(Fake::default());
    let reply = listing(256, 1500);
    let first = json!({"coordinate": "pkg::semantic::callee"});
    server.product.surface_replies.push_back(reply.clone());
    let result = call(&mut server, "backend.references", &first);
    assert_eq!(result["isError"], false, "{result}");
    assert_context_bounded(&result);
    let page = &result["structuredContent"]["references_page"];
    let returned = page["returned"].as_u64().expect("returned");
    assert!(returned >= 1 && returned < 25, "the default page was halved to fit: {page}");
    assert_eq!(page["remaining"].as_u64(), Some(256 - returned), "{page}");
    assert!(
        result["structuredContent"]["continuation_call"]["arguments"]["limit"].as_u64().is_some_and(|limit| limit < 25),
        "the continuation keeps the page size that fit"
    );
    let (_, starts) = read_all(&mut server, &reply, first);
    assert_eq!(starts, (0..256).collect::<Vec<u64>>());
}

#[test]
fn a_cursor_of_another_listing_is_refused() {
    let mut server = ready(Fake::default());
    server.product.surface_replies.push_back(listing(7, 0));
    let result = call(
        &mut server,
        "backend.references",
        &json!({"coordinate": "pkg::semantic::callee", "limit": 3}),
    );
    let mut next = result["structuredContent"]["continuation_call"]["arguments"].clone();
    server.product.surface_replies.push_back(listing(8, 0));
    let response = request(
        &mut server,
        "tools/call",
        &json!({"name": "backend.references", "arguments": next.clone()}),
    );
    assert!(
        response.get("error").is_some() || response["result"]["isError"] == true,
        "a changed listing is never stitched: {response}"
    );
    // A cursor is also bound to the declaration it was issued for.
    server.product.surface_replies.push_back(listing(7, 0));
    next["coordinate"] = json!("pkg::semantic::other");
    let response = request(
        &mut server,
        "tools/call",
        &json!({"name": "backend.references", "arguments": next}),
    );
    assert!(
        response.get("error").is_some() || response["result"]["isError"] == true,
        "{response}"
    );
}

#[test]
fn the_oversize_advice_names_only_arguments_the_tool_has() {
    // The raw surface tool has neither a limit nor a lower detail at summary.
    let references = match listing(256, 1500) {
        SurfaceReply::References { references, .. } => references,
        _ => unreachable!(),
    };
    let mut server = ready(Fake {
        surface_reply: Some(SurfaceReply::References {
            target: target(),
            coverage: backend_library::ReferenceCoverage::default(),
            references,
        }),
        ..Fake::default()
    });
    let surface = call(
        &mut server,
        SURFACE_TOOL,
        &json!({"command": serde_json::to_value(SurfaceCommand::References { target: target() }).expect("command")}),
    );
    assert_eq!(surface["structuredContent"]["cause"], "oversized");
    let detail = surface["structuredContent"]["detail"].as_str().expect("detail");
    assert!(!detail.contains("limit"), "{detail}");
    assert!(!detail.contains("detail=summary"), "already at summary: {detail}");
}
