//! Real source capture, persisted owner, public command codec and cold reopen.
use super::*;
use backend_library::{
    CommandDto, CommandReply, DocumentQuery, SourceRangeRequest, SourceRangeScope,
};

fn index_fixture(fixture: &mut AdapterFixture) {
    let label = fixture.label.clone();
    let (adapter, daemon) = fixture.parts();
    let operation = backend_library::IndexOperationKey::from_bytes([0x9a; 32]).expect("operation");
    adapter
        .start_index_operation(
            daemon,
            operation,
            backend_library::PackageReference::parse(label).expect("package"),
            CompileExecutionIntent::Interactive,
            0x9a,
        )
        .expect("real index request");
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while adapter.indexing.is_some() {
        adapter.poll_deferred(daemon);
        assert!(
            std::time::Instant::now() < deadline,
            "structural index terminal"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    finish_search_lane(adapter, daemon);
}

fn source_command(
    fixture: &mut AdapterFixture,
    name: &str,
    range: Option<SourceRangeRequest>,
) -> CommandReply {
    let (adapter, daemon) = fixture.parts();
    let view = daemon.engine().daemon().library().view();
    let rows = view
        .row_refs()
        .filter(|row| row.label.rsplit("::").next() == Some(name))
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 1, "unique captured source declaration {name}");
    let label = rows[0].label.to_string();
    let capability = view.capability();
    let root = daemon.engine().daemon().library().revision_root();
    let mut query = DocumentQuery::new(backend_engine::symbol_key(&label), root);
    if let Some(range) = range {
        query = query.with_source_range(range);
    }
    let request = CommandDto::new(0x9b, Command::Source(query)).with_certificate(
        symbol_coordinate_certificate(&label).with_claim(WireClaim::RootCommitment {
            schema: backend_engine::WireSchema::ViewRelation,
            id: backend_engine::encode_id(root.as_bytes()),
        }),
    );
    let body =
        backend_library::encode_command_body(&request).expect("public source request encoding");
    let bytes = adapter
        .execute(daemon, &body)
        .expect("public source dispatch");
    let reply = backend_library::ReplyDto::decode_with_certificate(&bytes, capability)
        .expect("strict public source reply decoding");
    backend_library::admit_reply(&request, &reply)
        .expect("public request/reply source correlation");
    reply.reply
}

#[test]
fn python_and_csharp_structural_source_survive_public_wire_and_cold_reopen() {
    let mut fixture = AdapterFixture::new();
    let root = PathBuf::from(&fixture.label);
    let python = "# λ before the source\napp = make_app()\n\n@app.get(\"/health\")\ndef health() -> dict[str, str]:\n    return {\"status\": \"ok\"}\n";
    let csharp = "// λ before the source\npublic class BankAccount {\n    private static bool MissingFields(string value) {\n        return value.Length == 0;\n    }\n}\n";
    fs::write(root.join("main.py"), python).expect("real Python source");
    fs::write(root.join("BankAccount.cs"), csharp).expect("real C# source");
    index_fixture(&mut fixture);
    for cold in [false, true] {
        if cold {
            fixture = fixture.reopen();
        }
        for (name, text) in [
            (
                "health",
                "@app.get(\"/health\")\ndef health() -> dict[str, str]:\n    return {\"status\": \"ok\"}",
            ),
            ("app", "app = make_app()"),
            (
                "MissingFields",
                "private static bool MissingFields(string value) {\n        return value.Length == 0;\n    }",
            ),
        ] {
            let CommandReply::Document(document) = source_command(&mut fixture, name, None) else {
                panic!("source must return {name}")
            };
            assert_eq!(document.excerpt.text(), Some(text));
            assert!(
                document.source_range.is_none(),
                "default source does not invent a range request"
            );
            assert!(
                document.source_policy.is_unknown(),
                "syntax capture is not a selected compiler-policy claim"
            );
            assert!(document.fragments.iter().any(|fragment| matches!(fragment, backend_library::Fragment::Text(note) if note == SourceRangeScope::StructuralDeclaration.description())), "default source explicitly qualifies structural capture");
        }
        for (name, expected) in [
            (
                "health",
                "@app.get(\"/health\")\ndef health() -> dict[str, str]:\n    return {\"status\": \"ok\"}",
            ),
            (
                "MissingFields",
                "private static bool MissingFields(string value) {\n        return value.Length == 0;\n    }",
            ),
        ] {
            let mut start = 0;
            let mut identity = None;
            let mut body = String::new();
            loop {
                let request = SourceRangeRequest {
                    start,
                    limit: 16,
                    source_identity: identity,
                };
                let CommandReply::Document(document) =
                    source_command(&mut fixture, name, Some(request))
                else {
                    panic!("source range reply")
                };
                let page = document.source_range.expect("bounded source page");
                assert!(page.admits(request));
                assert_eq!(page.scope, SourceRangeScope::StructuralDeclaration);
                body.push_str(&page.text);
                identity = Some(page.source_identity);
                start = page.end;
                if page.end == page.total {
                    break;
                }
            }
            assert_eq!(body, expected);
        }
    }
    fs::write(
        root.join("main.py"),
        python.replace("\"ok\"", "\"changed\""),
    )
    .expect("unindexed source edit");
    assert!(
        matches!(source_command(&mut fixture, "health", None), CommandReply::Failed(backend_library::CommandFailure::InvalidQuery(ref reason)) if reason.contains("changed"))
    );
}
