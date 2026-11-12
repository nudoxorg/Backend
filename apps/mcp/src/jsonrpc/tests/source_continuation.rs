//! Public MCP range routing fixture. This is not a native daemon journey.
use super::*;

#[test]
fn source_range_reconstructs_unicode_crlf_and_rejects_foreign_and_stale_cursors() {
    let original = format!(
        "function ferris() {{\r\n{}\r\n}}\r\n",
        "  const value = \"é漢🦀\";\r\n".repeat(200)
    );
    let mut server = ready(Fake {
        source_range_bytes: Some(original.clone()),
        ..Fake::default()
    });
    let mut output = String::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut args = json!({"byte_range": true, "coordinate": DECLARATION});
        if let Some(value) = &cursor {
            args["cursor"] = json!(value);
        }
        let response = call(&mut server, "backend.source", &args);
        assert_eq!(response["isError"], false, "{response}");
        assert_eq!(
            response["structuredContent"]["source"]["scope"], "unknown",
            "legacy fake must not gain native proof"
        );
        assert!(
            response["content"][0]["text"]
                .as_str()
                .expect("MCP text")
                .contains("Scope unknown")
        );
        assert!(
            response["content"][0]["text"]
                .as_str()
                .expect("MCP text")
                .contains("ignore-policy stability is not established")
        );
        output.push_str(
            response["structuredContent"]["source"]["text"]
                .as_str()
                .expect("raw text"),
        );
        cursor = response["structuredContent"]["nextCursor"]
            .as_str()
            .map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(output.as_bytes(), original.as_bytes());
    let first = call(
        &mut server,
        "backend.source",
        &json!({"byte_range":true,"coordinate":DECLARATION}),
    );
    let cursor = first["structuredContent"]["nextCursor"]
        .as_str()
        .expect("continuation")
        .to_owned();
    let foreign = request(
        &mut server,
        "tools/call",
        &json!({"name":"backend.source","arguments":{"byte_range":true,"coordinate":MISSING,"cursor":cursor}}),
    );
    assert!(foreign.get("error").is_some() || foreign["result"]["isError"] == true);
    let mut restarted = ready(Fake {
        source_range_bytes: Some(original.clone()),
        ..Fake::default()
    });
    let resumed = call(
        &mut restarted,
        "backend.source",
        &json!({"byte_range":true,"coordinate":DECLARATION,"cursor":cursor}),
    );
    assert_eq!(resumed["isError"], false, "MCP restart: {resumed}");
    let tampered = format!("{cursor}0");
    let rejected = request(
        &mut restarted,
        "tools/call",
        &json!({"name":"backend.source","arguments":{"byte_range":true,"coordinate":DECLARATION,"cursor":tampered}}),
    );
    assert!(rejected.get("error").is_some() || rejected["result"]["isError"] == true);
    server.product.stale_cursor = true;
    let stale = request(
        &mut server,
        "tools/call",
        &json!({"name":"backend.source","arguments":{"byte_range":true,"coordinate":DECLARATION,"cursor":cursor}}),
    );
    assert!(stale.get("error").is_some() || stale["result"]["isError"] == true);
}

#[test]
fn source_range_reconstructs_a_long_single_unicode_line() {
    let original = format!("const ferris = \"{}\";", "🦀é漢".repeat(1800));
    let mut server = ready(Fake {
        source_range_bytes: Some(original.clone()),
        ..Fake::default()
    });
    let mut output = String::new();
    let mut cursor = None;
    loop {
        let mut args = json!({"byte_range": true, "coordinate": DECLARATION});
        if let Some(value) = &cursor {
            args["cursor"] = json!(value);
        }
        let response = call(&mut server, "backend.source", &args);
        assert_eq!(response["isError"], false, "{response}");
        let page = response["structuredContent"]["source"]["text"]
            .as_str()
            .expect("raw source page");
        assert!(page.len() <= 4096);
        output.push_str(page);
        cursor = response["structuredContent"]["nextCursor"]
            .as_str()
            .map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(output.as_bytes(), original.as_bytes());
}

#[test]
fn source_range_rejects_mid_codepoint_and_accepts_exact_eof() {
    let original = "function ferris() { return \"é\"; }\r\n";
    let mut server = ready(Fake {
        source_range_bytes: Some(original.to_owned()),
        ..Fake::default()
    });
    let args = json!({"byte_range": true, "coordinate": DECLARATION});
    let context = continuation_context(
        PROJECT,
        "backend.source",
        args.as_object().expect("args"),
        Detail::Standard,
    );
    let make_cursor = |server: &Server<Fake>, offset: u32| {
        let mut payload = Vec::new();
        payload.extend_from_slice(view_state_root(&[]).as_bytes());
        payload.extend_from_slice(&[7; 32]);
        payload.extend_from_slice(&offset.to_be_bytes());
        server
            .sign_cursor_token_at(
                u64::MAX,
                &format!("sr1-{}", URL_SAFE_NO_PAD.encode(payload)),
                &context,
            )
            .expect("cursor")
    };
    let mid = u32::try_from(original.find('é').expect("Unicode") + 1).expect("offset");
    let invalid = make_cursor(&server, mid);
    let rejected = request(
        &mut server,
        "tools/call",
        &json!({"name":"backend.source","arguments":{"byte_range":true,"coordinate":DECLARATION,"cursor":invalid}}),
    );
    assert!(
        rejected.get("error").is_some() || rejected["result"]["isError"] == true,
        "{rejected}"
    );
    let eof = make_cursor(&server, u32::try_from(original.len()).expect("length"));
    let response = call(
        &mut server,
        "backend.source",
        &json!({"byte_range":true,"coordinate":DECLARATION,"cursor":eof}),
    );
    assert_eq!(response["isError"], false, "{response}");
    assert_eq!(response["structuredContent"]["source"]["text"], "");
    assert!(response["structuredContent"]["nextCursor"].is_null());
}

#[test]
fn source_page_remains_bounded_when_json_must_escape_every_byte() {
    let mut server = ready(Fake {
        source_range_bytes: Some("\0".repeat(5000)),
        ..Fake::default()
    });
    let response = request(
        &mut server,
        "tools/call",
        &json!({"name":"backend.source","arguments":{"byte_range":true,"coordinate":DECLARATION}}),
    );
    assert_eq!(response["result"]["isError"], false, "{response}");
    assert_context_bounded(&response);
    assert_eq!(
        response["result"]["structuredContent"]["source"]["text"]
            .as_str()
            .expect("source")
            .len(),
        2048
    );
    assert!(response["result"]["structuredContent"]["nextCursor"].is_string());
}

#[test]
fn registered_source_continuation_preserves_opt_in_and_selected_operands() {
    let mut server = ready(Fake {
        source_range_bytes: Some("é".repeat(4000)),
        ..Fake::default()
    });
    let first = call(
        &mut server,
        "backend.call",
        &json!({"name":"backend.source","arguments":{"coordinate":DECLARATION,"byte_range":true}}),
    );
    assert_eq!(first["isError"], false, "{first}");
    let next = &first["structuredContent"]["continuation_call"];
    assert_eq!(next["name"], "backend.call");
    assert_eq!(next["arguments"]["name"], "backend.source");
    assert_eq!(next["arguments"]["arguments"]["byte_range"], true);
    assert_eq!(next["arguments"]["arguments"]["coordinate"], DECLARATION);
    let second = request(&mut server, "tools/call", next);
    assert_eq!(second["result"]["isError"], false, "{second}");
    assert_eq!(
        second["result"]["structuredContent"]["source"]["start"],
        2048
    );
}

#[test]
fn source_scope_is_forwarded_from_owner_and_empty_native_extent_refused() {
    let original = "export function ferris() { return \"é漢🦀\"; }\r\n";
    let mut server = ready(Fake {
        source_range_bytes: Some(original.to_owned()),
        source_range_scope: backend_library::SourceRangeScope::NativePrimaryDeclaration,
        ..Fake::default()
    });
    for tool in ["backend.source", "backend.call"] {
        let args = json!({"byte_range":true,"coordinate":DECLARATION});
        let arguments = if tool == "backend.call" {
            json!({"name":"backend.source","arguments":args})
        } else {
            args
        };
        let response = call(&mut server, tool, &arguments);
        assert_eq!(response["isError"], false, "{response}");
        assert_eq!(response["structuredContent"]["source"]["text"], original);
        assert_eq!(
            response["structuredContent"]["source"]["scope"],
            "native_primary_declaration"
        );
        assert!(response["structuredContent"]["nextCursor"].is_null());
        assert!(
            response["content"][0]["text"]
                .as_str()
                .expect("text")
                .contains("modifiers or merged parts may be outside")
        );
        assert_context_bounded(&response);
    }
    server.product.source_range_bytes = Some(String::new());
    let refused = request(
        &mut server,
        "tools/call",
        &json!({"name":"backend.source","arguments":{"byte_range":true,"coordinate":DECLARATION}}),
    );
    assert!(
        refused.get("error").is_some() || refused["result"]["isError"] == true,
        "{refused}"
    );
}

#[test]
fn source_full_envelope_rejects_oversized_correlated_result() {
    let huge_id = json!("x".repeat(MCP_RESULT_BUDGET_BYTES));
    let result = tool_result(
        "é\r\n",
        json!({"source":{"text":"é\r\n","scope":"unknown"}}),
        false,
    );
    assert!(
        wire_admission::ResponseBudget::new(&huge_id)
            .admit(result)
            .is_err()
    );
}

#[test]
fn same_file_source_cursor_rejects_a_different_selected_symbol() {
    // The fake intentionally returns the same owner bytes and source identity
    // for either selector. Only the authenticated request context distinguishes
    // these same-file declarations at the wrapper boundary.
    let mut server = ready(Fake {
        source_range_bytes: Some("é漢🦀\r\n".repeat(600)),
        ..Fake::default()
    });
    let first_id = backend_library::symbol_key("same-file::alpha");
    let second_id = backend_library::symbol_key("same-file::bravo");
    let first = json!({"kind":"selected-symbol-id", "value":first_id.as_bytes()});
    let second = json!({"kind":"selected-symbol-id", "value":second_id.as_bytes()});
    let page = call(
        &mut server,
        "backend.source",
        &json!({"byte_range":true,"coordinate":DECLARATION,"selected_symbol":first}),
    );
    assert_eq!(page["isError"], false, "first source page: {page}");
    let cursor = page["structuredContent"]["nextCursor"]
        .as_str()
        .expect("cursor");
    let crossed = request(
        &mut server,
        "tools/call",
        &json!({"name":"backend.source","arguments":{
            "byte_range":true,"coordinate":DECLARATION,"selected_symbol":second,"cursor":cursor
        }}),
    );
    assert!(
        crossed.get("error").is_some() || crossed["result"]["isError"] == true,
        "cross-selected cursor: {crossed}"
    );
}

fn unverified_source_policy() -> backend_library::SourcePolicyQualification {
    use backend_library::{
        DocumentSelection, SemanticLanguageProfile, SourcePolicyObservation,
        SourcePolicyQualification, object_version,
    };
    let basis = Basis::new(view_state_root(&[]), object_version(b"fixture"));
    let row = Row::new(RowId::Symbol(symbol_key(MODULE)), basis, MODULE);
    let selection = DocumentSelection::from_row(&row, basis).expect("selection");
    let profile = SemanticLanguageProfile::from_name("typescript").expect("profile");
    SourcePolicyQualification::issue_observed(
        &[12; 32],
        &selection,
        SourcePolicyObservation {
            workspace_root: [1; 32],
            evidence_root: [2; 32],
            project: [3; 32],
            profile,
            publication_profile: profile,
            publication_coordinate: "pkg:npm/fixture@1.0.0".to_owned(),
            source_path: "source.ts".to_owned(),
            source_fact: [7; 32],
            input_digest: [5; 32],
            byte_len: 12,
            publication_binding: [6; 32],
            image_identity: [7; 32],
            image_byte_len: 128,
            program_membership: [8; 32],
            entity_index: 9,
            declaration_family: [10; 16],
            declaration_variant: [11; 16],
        },
    )
    .expect("unverified fixture claim")
}

#[test]
fn unadmitted_qualified_source_is_refused_even_with_matching_geometry() {
    let policy = unverified_source_policy();
    for mismatch in [false, true] {
        let mut server = ready(Fake {
            source_range_bytes: Some("é漢🦀\r\n".to_owned()),
            source_range_policy: Some(policy.clone()),
            source_range_policy_mismatch: mismatch,
            ..Fake::default()
        });
        for target in [
            json!({"name":"backend.source","arguments":{"byte_range":true,"coordinate":DECLARATION}}),
            json!({"name":"backend.call","arguments":{"name":"backend.source","arguments":{"byte_range":true,"coordinate":DECLARATION}}}),
        ] {
            let response = request(&mut server, "tools/call", &target);
            assert!(
                response.get("error").is_some() || response["result"]["isError"] == true,
                "raw claim accepted without authenticated session: {response}"
            );
        }
    }
}


#[test]
fn source_reconnect_refuses_unmatched_replacement_and_never_reuses_prior_bytes() {
    struct Replacement(Option<Fake>);
    impl super::super::reconnect::Endpoint for Replacement {
        type Product = Fake;

        fn connect(&mut self, _previous: Option<Fake>) -> Result<Fake, ClientError> {
            self.0.take().ok_or(ClientError::Disconnected(std::io::ErrorKind::NotConnected))
        }
    }

    // This sentinel simulates an eligible retired lease only. Its one successful
    // response is legacy Unknown; no qualified fake reply is accepted as proof.
    for replacement in [true, false] {
        let old = Fake {
            source_range_bytes: Some("prior-only-秘密\r\n".repeat(500)),
            source_range_lease_sentinel: true,
            source_range_disconnect_after: Some(1),
            ..Fake::default()
        };
        let unmatched = replacement.then(|| Fake {
            source_range_bytes: Some("replacement-must-not-leak\r\n".repeat(500)),
            source_range_policy: Some(unverified_source_policy()),
            ..Fake::default()
        });
        let mut server = ready(super::super::reconnect::Reconnecting::new(
            Replacement(unmatched), old,
        ));
        let first = call(&mut server, "backend.source", &json!({
            "byte_range": true, "coordinate": DECLARATION
        }));
        assert_eq!(first["isError"], false, "legacy control: {first}");
        assert_eq!(first["structuredContent"]["source"]["scope"], "unknown");
        let cursor = first["structuredContent"]["nextCursor"].as_str().expect("cursor");
        let second = request(&mut server, "tools/call", &json!({
            "name": "backend.source", "arguments": {
                "byte_range": true, "coordinate": DECLARATION, "cursor": cursor
            }
        }));
        assert!(second.get("error").is_some() || second["result"]["isError"] == true, "{second}");
        let encoded = second.to_string();
        assert!(!encoded.contains("prior-only"), "previous page leaked: {second}");
        assert!(!encoded.contains("replacement-must-not-leak"), "unmatched page leaked: {second}");
    }
}

#[test]
fn source_error_after_good_page_does_not_return_the_previous_excerpt() {
    let mut server = ready(Fake {
        // This only exercises error propagation with the lease sentinel set.
        source_range_lease_sentinel: true,
        source_range_bytes: Some("prior-only-秘密\r\n".repeat(500)),
        ..Fake::default()
    });
    let first = call(&mut server, "backend.source", &json!({
        "byte_range": true, "coordinate": DECLARATION
    }));
    assert_eq!(first["isError"], false, "{first}");
    let cursor = first["structuredContent"]["nextCursor"].as_str().expect("cursor");
    server.product.stale_cursor = true;
    for name in ["backend.source", "backend.call"] {
        let args = json!({"byte_range": true, "coordinate": DECLARATION, "cursor": cursor});
        let arguments = if name == "backend.call" {
            json!({"name": "backend.source", "arguments": args})
        } else {
            args
        };
        let response = request(&mut server, "tools/call", &json!({"name": name, "arguments": arguments}));
        assert!(response.get("error").is_some() || response["result"]["isError"] == true, "{response}");
        assert!(!response.to_string().contains("prior-only"), "stale source leaked: {response}");
    }
}

#[test]
fn nested_policy_mismatch_refuses_even_with_lease_sentinel() {
    // The sentinel exercises the ordering of the two checks only; it is not
    // an authenticated session and must never produce a qualified success.
    let mut server = ready(Fake {
        source_range_bytes: Some("mismatched-must-not-leak".to_owned()),
        source_range_policy: Some(unverified_source_policy()),
        source_range_policy_mismatch: true,
        source_range_lease_sentinel: true,
        ..Fake::default()
    });
    for target in [
        json!({"name":"backend.source","arguments":{"byte_range":true,"coordinate":DECLARATION}}),
        json!({"name":"backend.call","arguments":{"name":"backend.source","arguments":{"byte_range":true,"coordinate":DECLARATION}}}),
    ] {
        let response = request(&mut server, "tools/call", &target);
        assert!(response.get("error").is_some() || response["result"]["isError"] == true, "{response}");
        assert!(response.to_string().contains("source policy differs between document and range"), "{response}");
        assert!(!response.to_string().contains("mismatched-must-not-leak"));
    }
}
