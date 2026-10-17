//! Complete MCP default-envelope controls; synthetic owner rows, no native claim.
use super::*;

#[test]
fn document_default_pages_all_sections_with_exact_identity_and_complete_envelope_budget() {
    let native = symbol_key(DECLARATION);
    let mut owner = declaration_row();
    owner.kind = Some(DeclarationKind::Class);
    let mut rows = vec![package_row(), module_row(), owner];
    for index in 0..80 {
        let label = format!("{DECLARATION}::member_{index:03}");
        rows.push(
            Row::in_package(
                RowId::Symbol(symbol_key(&label)),
                basis(),
                package_key(PROJECT),
                &label,
            )
            .with_parent(native)
            .with_kind(DeclarationKind::Method)
            .with_signature(format!(
                "fn member_{index:03}(&self, document: Document) -> Document"
            )),
        );
    }
    let view = root(rows);
    let catalog = backend_library::Library::from_view(
        view.clone(),
        backend_library::Cursor::for_view_root(&view),
    )
    .expect("catalog");
    let mut selected = document();
    selected.symbol = native;
    selected.basis = catalog.revision_root();
    selected = selected.with_source_basis(Basis {
        root: catalog.revision_root(),
        ..basis()
    });
    let mut server = ready(Fake {
        page_catalog: Some(catalog),
        document_override: Some(selected),
        ..Fake::default()
    });
    let mut arguments = json!({"coordinate": DECLARATION});
    let mut seen = std::collections::BTreeSet::new();
    let mut offset = 0;
    loop {
        let response = request(
            &mut server,
            "tools/call",
            &json!({"name":"backend.document", "arguments":arguments}),
        );
        let response = encoded_stdio_reply(response);
        assert!(
            serialized_bytes(&response) + 1 <= MCP_RESULT_BUDGET_BYTES,
            "whole MCP envelope"
        );
        assert!(wire_tokens(&response).expect("whole-envelope tokens") <= MCP_CL100K_TOKEN_LIMIT);
        let result = &response["result"];
        assert_eq!(result["isError"], false, "{response}");
        let page = &result["structuredContent"];
        assert_eq!(page["identity"]["coordinate"], DECLARATION);
        assert_eq!(
            page["identity"]["semantic_data"]["value"],
            json!(native.as_bytes())
        );
        assert!(page["signature"].is_string());
        assert!(page["source"].is_object());
        assert_eq!(page["section_page"]["start"], offset);
        offset = page["section_page"]["end"].as_u64().expect("extent");
        for group in page["members"].as_array().expect("member groups") {
            for row in group["members"].as_array().expect("members") {
                assert!(
                    seen.insert(
                        row["identity"]["coordinate"]
                            .as_str()
                            .expect("exact coordinate")
                            .to_owned()
                    )
                );
            }
        }
        let Some(next) = page["section_page"]["next_cursor"].as_str() else {
            break;
        };
        assert!(
            text_of(result).contains(next),
            "copyable continuation in readable output"
        );
        arguments["cursor"] = json!(next);
        assert!(seen.len() < 81);
    }
    assert_eq!(seen.len(), 80);
}
