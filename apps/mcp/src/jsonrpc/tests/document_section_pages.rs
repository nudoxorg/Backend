//! Complete MCP default-envelope controls; synthetic owner rows, no native claim.
use super::*;

#[test]
fn document_default_pages_all_sections_with_exact_identity_and_complete_envelope_budget() {
    let native = symbol_key(DECLARATION);
    let mut owner = declaration_row();
    owner.kind = Some(DeclarationKind::Class);
    owner.signature = Some("pub mod lib".to_owned());
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
        arguments = page["section_page"]["continuation"].clone();
        assert_eq!(
            arguments["selected_symbol"], page["identity"]["semantic_data"],
            "MCP continuation retains all selected bytes"
        );
        assert_eq!(arguments["coordinate"], DECLARATION);
        assert!(seen.len() < 81);
    }
    assert_eq!(seen.len(), 80);
    assert_eq!(
        server.product.document_coordinates,
        [DECLARATION],
        "only the first legacy call used coordinate lookup; every continuation retained the selected ID"
    );
}

#[test]
fn document_continuation_never_falls_back_between_same_coordinate_selected_rows() {
    let first = symbol_key("same-coordinate-first-class");
    let second = symbol_key("same-coordinate-second-class");
    let mut rows = vec![package_row(), module_row()];
    for (selected, family) in [(first, "first"), (second, "second")] {
        rows.push(
            Row::in_package(
                RowId::Symbol(selected),
                basis(),
                package_key(PROJECT),
                DECLARATION,
            )
            .with_kind(DeclarationKind::Class)
            .with_signature(format!("class {family}:")),
        );
        for index in 0..24 {
            let label = format!(
                "{PROJECT}::src/lib.rs:{}::{family}_member_{index:02}",
                index + 10
            );
            rows.push(
                Row::in_package(
                    RowId::Symbol(symbol_key(&label)),
                    basis(),
                    package_key(PROJECT),
                    &label,
                )
                .with_parent(selected)
                .with_kind(DeclarationKind::Method),
            );
        }
    }
    let view = root(rows);
    let catalog = backend_library::Library::from_view(
        view.clone(),
        backend_library::Cursor::for_view_root(&view),
    )
    .expect("current two-row owner fixture");
    let mut server = ready(Fake {
        page_catalog: Some(catalog),
        ..Fake::default()
    });
    let selected = |key: backend_library::SymbolKey| json!({"kind":"selected-symbol-id","value":key.as_bytes()});
    for (native, family, wrong) in [(first, "first", second), (second, "second", first)] {
        let result = call(
            &mut server,
            "backend.document",
            &json!({"coordinate":DECLARATION,"selected_symbol":selected(native)}),
        );
        assert_eq!(result["isError"], false, "{result}");
        let next = result["structuredContent"]["section_page"]["continuation"].clone();
        assert_eq!(next["selected_symbol"], selected(native));
        assert_eq!(next["coordinate"], DECLARATION);
        let resumed = call(&mut server, "backend.document", &next);
        assert_eq!(resumed["isError"], false, "{resumed}");
        assert_eq!(
            resumed["structuredContent"]["identity"]["semantic_data"],
            selected(native)
        );
        for group in resumed["structuredContent"]["members"]
            .as_array()
            .expect("members")
        {
            for member in group["members"].as_array().expect("members") {
                assert!(
                    member["identity"]["name"]
                        .as_str()
                        .expect("name")
                        .starts_with(family)
                );
            }
        }
        let mut switched = next.clone();
        switched["selected_symbol"] = selected(wrong);
        assert_eq!(
            call(&mut server, "backend.document", &switched)["isError"],
            true,
            "another same-coordinate row cannot use this cursor"
        );
        let mut absent = next.clone();
        absent["selected_symbol"] = selected(symbol_key("absent-selected-row"));
        assert_eq!(
            call(&mut server, "backend.document", &absent)["isError"],
            true,
            "missing selected row cannot fall back to coordinate"
        );
        let mut coordinate_only = next;
        coordinate_only
            .as_object_mut()
            .expect("arguments")
            .remove("selected_symbol");
        assert_eq!(
            call(&mut server, "backend.document", &coordinate_only)["isError"],
            true,
            "coordinate-only resume refused"
        );
    }
    assert!(
        server.product.document_coordinates.is_empty(),
        "not one selected continuation used the coordinate document probe"
    );
}
