//! These tests exercise the existing selected-view read and real command
//! codecs. They do not certify compiler extraction or repair the reported
//! installed endpoint timeouts. Ambiguous-label selection remains a named
//! compatibility limitation in this I/O-boundary change.

use super::*;
use backend_library::{
    CommandDto, CommandReply, DeclarationReadExtent, ProductText, Row, RowId, SourceExcerpt,
    SurfaceCommand, SurfaceReply,
};

fn read_request(id: u64, labels: &[&str]) -> CommandDto {
    CommandDto::new(
        id,
        Command::Surface(SurfaceCommand::Read {
            locators: labels
                .iter()
                .map(|label| ProductText::new(*label).expect("bounded locator"))
                .collect(),
        }),
    )
}

fn read_through_wire(
    adapter: &mut CommandAdapter,
    daemon: &mut ProductDaemon,
    request: &CommandDto,
) -> Box<[backend_library::DeclarationRecord]> {
    let body = backend_library::encode_command_body(request).expect("strict request encoding");
    let bytes = adapter
        .execute(daemon, &body)
        .expect("selected read dispatch");
    let decoded = backend_library::decode_reply_body(&bytes).expect("strict reply decode");
    backend_library::admit_reply(request, &decoded).expect("exact read request/reply admission");
    assert_eq!(decoded.request_id, request.request_id);
    let CommandReply::Surface(SurfaceReply::Read(records)) = decoded.reply else {
        panic!("typed batch read reply");
    };
    records
}

fn align_fixture_sql(adapter: &mut CommandAdapter, daemon: &ProductDaemon) {
    super::super::super::super::sql_projection::synchronize_current(
        adapter
            .sql_projection
            .writer_mut()
            .expect("unique SQL writer"),
        daemon,
    )
    .expect("align real SQL to checked fixture view before refusal controls");
}

#[test]
fn local_owner_explicitly_answers_outline_witness_route_probe_with_revision() {
    let mut fixture = AdapterFixture::new();
    let request = CommandDto::new(u64::MAX - 1, Command::OutlineOwnerWitnessCapability);
    let (adapter, daemon) = fixture.parts();
    let body = backend_library::encode_command_body(&request).expect("probe request encoding");
    let bytes = adapter
        .execute(daemon, &body)
        .expect("local product owner answers capability probe");
    let reply = backend_library::decode_reply_body(&bytes).expect("actual owner reply decode");
    backend_library::admit_reply(&request, &reply).expect("correlated owner capability reply");
    assert!(matches!(reply.reply, CommandReply::Revision(_)));
}

#[test]
fn local_batch_read_ignores_invalid_unrelated_manifest_and_unconfigured_catalog() {
    let mut fixture = AdapterFixture::new();
    let project = PathBuf::from(&fixture.label);
    let basis = fixture
        .daemon
        .as_ref()
        .expect("owner")
        .engine()
        .daemon()
        .library()
        .view()
        .basis();
    let router = backend_library::symbol_key("native-selected-router");
    let lowercase = backend_library::symbol_key("native-selected-lowercase-router");
    let signature = backend_library::symbol_key("native-selected-signature");
    let long_excerpt = SourceExcerpt::capture_bounded(&format!(
        "type Router struct {{ /* {} */ }}",
        "λ".repeat(SourceExcerpt::MAX_BYTES),
    ));
    assert_eq!(
        long_excerpt.extent(),
        Some(backend_library::SourceExcerptExtent::Truncated)
    );
    fixture.publish_rows(vec![
        Row::new(RowId::Symbol(router), basis, "Router")
            .with_signature("type Router struct")
            .with_excerpt(long_excerpt.clone()),
        Row::new(RowId::Symbol(lowercase), basis, "router")
            .with_excerpt(SourceExcerpt::capture_bounded("func router() {}")),
        Row::new(RowId::Symbol(signature), basis, "signature-only")
            .with_signature("type SignatureOnly interface")
            .with_excerpt(SourceExcerpt::NotHydrated),
    ]);
    let (adapter, daemon) = fixture.parts();
    align_fixture_sql(adapter, daemon);
    assert!(
        adapter.registry.is_none(),
        "catalog authority is unconfigured"
    );
    fs::write(project.join("go.mod"), "module\n").expect("actual invalid dependency manifest");
    let before = owner_cursor(daemon);
    let before_basis = daemon.engine().daemon().library().view().basis();
    let before_sql = adapter
        .sql_projection
        .revision()
        .expect("real SQL revision");
    let before_witness = adapter.manifests.witness();
    // Prove the unrelated input is genuinely refused, without routing the
    // batch read through a synthesized empty dependency graph or catalog.
    let dependency_error =
        super::super::super::super::local_manifest::local_dependency_facts(&project)
            .expect_err("malformed local dependency authority must fail");
    assert!(dependency_error.contains("module path"));
    let records = read_through_wire(
        adapter,
        daemon,
        &read_request(
            1101,
            &[
                "Router",
                "router",
                "signature-only",
                "ROUTER",
                "missing",
                "Router",
            ],
        ),
    );
    assert_eq!(records.len(), 6);
    assert_eq!(records[0].stable_id, Some(router.to_bytes()));
    assert_eq!(records[0].extent, Some(DeclarationReadExtent::Truncated));
    assert_eq!(
        records[0].signature.as_ref().map(ProductText::as_str),
        long_excerpt.text()
    );
    assert_eq!(records[1].stable_id, Some(lowercase.to_bytes()));
    assert_eq!(records[1].extent, Some(DeclarationReadExtent::Complete));
    assert_eq!(
        records[1].signature.as_ref().map(ProductText::as_str),
        Some("func router() {}")
    );
    assert_eq!(records[2].stable_id, Some(signature.to_bytes()));
    assert_eq!(records[2].extent, None);
    assert_eq!(
        records[2].signature.as_ref().map(ProductText::as_str),
        Some("type SignatureOnly interface")
    );
    for index in [3, 4] {
        assert_eq!(records[index].stable_id, None);
        assert_eq!(records[index].signature, None);
        assert_eq!(records[index].extent, None);
    }
    assert_eq!(
        records[5], records[0],
        "batch order and repeated locators survive"
    );
    assert_eq!(owner_cursor(daemon), before);
    assert_eq!(
        daemon.engine().daemon().library().view().basis(),
        before_basis
    );
    assert_eq!(
        adapter
            .sql_projection
            .revision()
            .expect("SQL remains readable"),
        before_sql
    );
    assert_eq!(
        adapter.manifests.witness(),
        before_witness,
        "Read never refreshes unrelated manifest authority"
    );
    assert!(
        adapter.dependencies.is_none(),
        "Read never builds an unrelated graph"
    );
    assert!(
        !daemon
            .engine()
            .daemon()
            .library()
            .view()
            .compatibility_rows_are_materialized()
    );
    let dependency_request = CommandDto::new(
        1102,
        Command::Surface(SurfaceCommand::Dependencies {
            package: backend_library::PackageReference::Local(
                ProductText::new(project.to_str().expect("UTF-8 path")).expect("bounded package"),
            ),
        }),
    );
    let body = backend_library::encode_command_body(&dependency_request)
        .expect("strict dependency request");
    let refused = adapter
        .execute(daemon, &body)
        .expect_err("dependency command still observes its invalid authority");
    assert!(refused.0.contains("module path"));
    assert_eq!(owner_cursor(daemon), before);
}

#[test]
fn local_read_uses_selected_view_when_unrelated_sql_writer_is_reserved() {
    let mut fixture = AdapterFixture::new();
    let label = fixture.label.clone();
    let package = fixture.package;
    let (adapter, daemon) = fixture.parts();
    let before = owner_cursor(daemon);
    let writer = adapter
        .sql_projection
        .reserve_writer()
        .expect("capture real read snapshot and transfer unique writer");
    assert!(matches!(
        adapter.sql_projection.writer_mut(),
        Err(backend_extension_turso::ProjectionError::NamespaceBusy)
    ));
    let records = read_through_wire(adapter, daemon, &read_request(1111, &[&label, "missing"]));
    assert_eq!(records[0].stable_id, Some(package.to_bytes()));
    assert_eq!(records[1].stable_id, None);
    assert!(adapter.dependencies.is_none());
    // The generic catalog-dependent path still requires its genuine writer;
    // no unconfigured/empty graph is silently promoted into authority.
    let command = SurfaceCommand::Dependencies {
        package: backend_library::PackageReference::Local(
            ProductText::new(label).expect("package"),
        ),
    };
    let error = adapter
        .surface(daemon, command, 1112)
        .expect_err("dependency graph publication requires writer");
    assert_eq!(
        error.0,
        backend_extension_turso::ProjectionError::NamespaceBusy.to_string()
    );
    let reader = adapter
        .sql_projection
        .prepare_return(writer)
        .unwrap_or_else(|(_, error)| panic!("return genuine SQL writer: {error}"))
        .install();
    drop(reader);
    assert_eq!(owner_cursor(daemon), before);
}

#[test]
fn batch_read_preserves_legacy_ambiguous_label_selection_without_claiming_unique_identity() {
    let mut fixture = AdapterFixture::new();
    let basis = fixture
        .daemon
        .as_ref()
        .expect("owner")
        .engine()
        .daemon()
        .library()
        .view()
        .basis();
    let first = backend_library::symbol_key("duplicate-first-native");
    let second = backend_library::symbol_key("duplicate-second-native");
    fixture.publish_rows(vec![
        Row::new(RowId::Symbol(first), basis, "duplicate").with_signature("first-signature"),
        Row::new(RowId::Symbol(second), basis, "duplicate").with_signature("second-signature"),
    ]);
    let (adapter, daemon) = fixture.parts();
    let view = daemon.engine().daemon().library().view();
    assert!(view.label_is_ambiguous("duplicate"));
    assert!(view.unique_symbol_by_label("duplicate").is_none());
    let expected = first.min(second);
    let expected_signature = if expected == first {
        "first-signature"
    } else {
        "second-signature"
    };
    let before = owner_cursor(daemon);
    let records = read_through_wire(adapter, daemon, &read_request(1121, &["duplicate"]));
    assert_eq!(records[0].stable_id, Some(expected.to_bytes()));
    assert_eq!(
        records[0].signature.as_ref().map(ProductText::as_str),
        Some(expected_signature)
    );
    assert_eq!(records[0].extent, None);
    assert_eq!(owner_cursor(daemon), before);
}

#[test]
fn local_read_preserves_atomic_invalid_text_failure_and_owner_basis() {
    let mut fixture = AdapterFixture::new();
    let basis = fixture.parts().1.engine().daemon().library().view().basis();
    let invalid_signature = "x".repeat(backend_library::MAX_PRODUCT_TEXT_BYTES + 1);
    let expected_error = ProductText::new(invalid_signature.clone())
        .expect_err("independent product text bound")
        .to_string();
    fixture.publish_rows(vec![
        Row::new(
            RowId::Symbol(backend_library::symbol_key("valid-batch-signature")),
            basis,
            "valid",
        )
        .with_signature("valid signature"),
        Row::new(
            RowId::Symbol(backend_library::symbol_key("invalid-batch-signature")),
            basis,
            "invalid",
        )
        .with_signature(invalid_signature),
    ]);
    let (adapter, daemon) = fixture.parts();
    let before = owner_cursor(daemon);
    let request = read_request(1122, &["valid", "invalid", "missing"]);
    let body = backend_library::encode_command_body(&request).expect("strict request encoding");
    let bytes = adapter.execute(daemon, &body).expect("typed read failure");
    let decoded = backend_library::decode_reply_body(&bytes).expect("strict failure reply decode");
    backend_library::admit_reply(&request, &decoded).expect("correlated failure admission");
    assert_eq!(decoded.request_id, request.request_id);
    assert_eq!(
        decoded.reply,
        CommandReply::Failed(backend_library::CommandFailure::InvalidQuery(
            expected_error
        )),
        "the original text refusal is retained; no partial successful batch escapes"
    );
    assert_eq!(owner_cursor(daemon), before);
    assert_eq!(daemon.engine().daemon().library().view().basis(), basis);
    assert!(adapter.dependencies.is_none());
}

#[test]
fn local_read_still_refuses_oversized_and_unknown_wire_claims_before_dispatch() {
    let mut fixture = AdapterFixture::new();
    let (adapter, daemon) = fixture.parts();
    let before = owner_cursor(daemon);
    let oversized = read_request(
        1131,
        &vec!["missing"; backend_library::MAX_PRODUCT_ROWS + 1],
    );
    let bytes = serde_json::to_vec(&oversized).expect("raw oversized input");
    let error = adapter
        .execute(daemon, &bytes)
        .expect_err("owner codec refuses collection bound");
    assert!(error.0.starts_with("decode command DTO:"));
    let request = read_request(1132, &["missing"]);
    let mut unknown = serde_json::to_value(&request).expect("request JSON");
    unknown
        .as_object_mut()
        .expect("request object")
        .insert("unknownClaim".to_owned(), serde_json::json!(true));
    let bytes = serde_json::to_vec(&unknown).expect("raw unknown field input");
    let error = adapter
        .execute(daemon, &bytes)
        .expect_err("owner codec refuses unknown fields");
    assert!(error.0.starts_with("decode command DTO:"));
    assert_eq!(owner_cursor(daemon), before);
    assert!(adapter.dependencies.is_none());
}
