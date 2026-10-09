//! Typed producer-row controls. These native IR values exercise presentation
//! lowering; they are not an application indexing or publication receipt.

#![allow(clippy::expect_used, clippy::panic)]

use crate::*;
use backend_client::ClientError;
use backend_library::{
    Basis, CommandReply, Document, Fragment, Freshness, Frontier, GraphRelation, HealthReport,
    Outline, OutlineNode, ReplyDto, Row, RowId, SemanticLinkKind, SourceAvailability,
    SourceExcerpt, SourceExcerptExtent, SourceLocation, SurfaceCommand, SurfaceReply, SymbolKey,
    ViewRoot, ViewSnapshot, ViewStateRoot, encode_id, object_version, package_key, symbol_key,
    view_key, view_state_root,
};
use backend_semantic::ir::{
    ExternalDeclarationIdentity, ExternalTarget, ExternalTargetIdentity, ForeignDeclarationId,
    ForeignExternalTarget, ForeignTargetOrigin, IrBuilder, VariantAvailability,
};

const PROJECT: &str = "/abs/python-project";

fn basis() -> Basis {
    Basis::new(view_state_root(&[]), object_version(b"row-identity-source"))
}

fn foreign_rows() -> [Row; 2] {
    let mut builder = IrBuilder::new();
    let ecosystem = builder.intern_atom(b"python").expect("native ecosystem");
    let path = builder
        .intern_atom(b"foreign.initialise")
        .expect("native path");
    let display = builder.intern_atom(b"initialise").expect("native display");
    let targets = [1_u8, 2].map(|identity| {
        builder
            .intern_external(ExternalTarget::Foreign(ForeignExternalTarget {
                identity: ExternalDeclarationIdentity {
                    foreign: ForeignDeclarationId::from_raw([identity; 16]),
                    variant: VariantAvailability::Unavailable,
                },
                origin: ForeignTargetOrigin::Unspecified { ecosystem },
                path,
                display,
                kind: None,
            }))
            .expect("native Foreign target")
    });
    let ir = builder.finish().expect("validated native Foreign pool");
    targets.map(|target| {
        let native = ExternalTargetIdentity::capture(&ir, target).expect("native endpoint");
        let preimage = encode_id(
            native
                .in_scope(package_key(PROJECT).to_bytes(), [7; 32])
                .as_bytes(),
        );
        // This is the existing producer's selected-key recipe, not a display-name key.
        Row::new(RowId::Symbol(symbol_key(&preimage)), basis(), "initialise")
            .try_with_identity_preimage(&preimage)
            .expect("native selected-row preimage")
    })
}

fn symbol(row: &Row) -> SymbolKey {
    let RowId::Symbol(key) = row.id else {
        panic!("symbol control")
    };
    key
}

fn assert_unattached(identity: &Identity, row: &Row) {
    assert_eq!(identity.shape(), IdentityShape::Symbol);
    assert_eq!(identity.key(), IdentityKey::Symbol(symbol(row)));
    assert_eq!(identity.coordinate().as_str(), row.label);
    assert_eq!(identity.name(), row.label);
    assert_eq!(identity.trail_within(None), row.label);
    assert_eq!(identity.project(), None);
    assert_eq!(identity.path(), None);
    assert_eq!(identity.line(), None);
    assert_eq!(identity.language(), Language::Unknown);
}

#[test]
fn native_foreign_keys_and_literal_displays_survive_both_shared_encoders() {
    let rows = foreign_rows();
    assert_ne!(
        rows[0].id, rows[1].id,
        "same native display, distinct endpoints"
    );
    for original in rows {
        for display in [
            "initialise".to_owned(),
            "/abs/fake::src/invented.py:999::initialise".to_owned(),
            format!("/abs/fake::semantic::{}::initialise", "ac".repeat(32)),
            "/abs/fake::external::invented".to_owned(),
        ] {
            let mut row = original.clone();
            row.label = display;
            let before = row.clone();
            let record = Record::from_row(&row);
            assert_unattached(record.identity(), &row);
            assert_eq!(record.kind(), None);
            let answer = Answer::Records(Box::new(RecordList::new(
                "initialise",
                CoverageLine::new(&[], Some(1)),
                vec![record],
            )));
            for detail in [Detail::Summary, Detail::Full] {
                let encoded = encode_answer(&answer, detail, None, DEFAULT_RESPONSE_BUDGET_BYTES)
                    .expect("actual shared CLI/MCP encoder");
                let value: serde_json::Value =
                    serde_json::from_slice(&encoded.bytes).expect("JSON");
                let identity = &value["records"][0]["identity"];
                assert_eq!(identity["shape"], "symbol");
                assert_eq!(identity["coordinate"], row.label);
                assert_eq!(identity["name"], row.label);
                assert_eq!(identity["segments"], serde_json::json!([row.label]));
                assert_eq!(
                    identity["semantic_data"],
                    serde_json::json!({
                        "kind": "selected-symbol-id", "value": symbol(&row).as_bytes(),
                    })
                );
                assert!(identity.get("project").is_none());
                assert!(identity.get("path").is_none());
                assert!(identity.get("line").is_none());
                assert!(value["records"][0].get("kind").is_none());
            }
            assert_eq!(row, before, "projection cannot rewrite native row evidence");
        }
    }
}

#[test]
fn local_rows_use_captured_sites_and_require_their_actual_package_key_for_scope() {
    let label = format!("{PROJECT}::src/display.rs:999::local");
    let mut row = Row::in_package(
        RowId::Symbol(symbol_key("native-local")),
        basis(),
        package_key(PROJECT),
        &label,
    )
    .with_kind(backend_library::DeclarationKind::Function)
    .with_source(SourceLocation::new("httpie/context.py", 46).expect("actual site"));
    let identity = Identity::from_row(&row);
    assert_eq!(identity.shape(), IdentityShape::Declaration);
    assert_eq!(identity.project().map(ProjectRef::root), Some(PROJECT));
    assert_eq!(
        identity.path().map(PackagePath::as_str),
        Some("httpie/context.py")
    );
    assert_eq!(identity.line().map(LineNumber::get), Some(46));
    assert_eq!(identity.language(), Language::Python);
    let module = Row::in_package(
        RowId::Symbol(symbol_key("native-module")),
        basis(),
        package_key(PROJECT),
        format!("{PROJECT}::src/module.py"),
    )
    .with_kind(backend_library::DeclarationKind::Module);
    let module_identity = Identity::from_row(&module);
    assert_eq!(module_identity.name(), "module.py");
    assert_eq!(module_identity.path(), None);
    assert_eq!(module_identity.line(), None);
    let mut external_spelling = row.clone();
    external_spelling.label = format!("{PROJECT}::external::display-only");
    let identity = Identity::from_row(&external_spelling);
    assert_eq!(
        identity.shape(),
        IdentityShape::Symbol,
        "package/source facts do not certify an external endpoint category"
    );
    assert_eq!(identity.project().map(ProjectRef::root), Some(PROJECT));
    assert_eq!(
        identity.path().map(PackagePath::as_str),
        Some("httpie/context.py")
    );
    assert_eq!(identity.key(), IdentityKey::Symbol(symbol(&row)));
    row.package = Some(package_key("/abs/another-project"));
    assert_eq!(Identity::from_row(&row).project(), None);
    row.source = SourceAvailability::NotCaptured;
    let identity = Identity::from_row(&row);
    assert_unattached(&identity, &row);
    row.package = Some(package_key(PROJECT));
    let identity = Identity::from_row(&row);
    assert_eq!(identity.project().map(ProjectRef::root), Some(PROJECT));
    assert_eq!(
        identity.path(),
        None,
        "a display path is not a captured source path"
    );
    assert_eq!(identity.line(), None);
}

#[test]
fn package_rows_and_outline_roots_keep_the_package_plane_even_with_delimiters() {
    for label in ["initialise", "/abs/package::with-delimiters"] {
        let package = package_key(label);
        let row = Row::new(RowId::Package(package), basis(), label);
        let identity = Identity::from_row(&row);
        assert_eq!(identity.shape(), IdentityShape::Package);
        assert_eq!(identity.key(), IdentityKey::Package(package));
        assert_eq!(identity.project().map(ProjectRef::root), Some(label));
        assert_eq!(identity.path(), None);
        assert_eq!(IdentityDto::new(&identity).semantic_data, None);
        let outline = Outline::new(
            package,
            basis().root,
            OutlineNode {
                symbol: symbol_key("unresolved"),
                children: Box::new([]),
            },
        );
        let tree = outline_tree(label, &outline, &[]);
        assert_eq!(tree.package(), &identity);
    }
}

#[test]
fn foreign_rows_do_not_create_shelf_projects_or_borrow_local_language_counts() {
    let mut rows = foreign_rows().to_vec();
    rows[0].label = format!("{PROJECT}::src/pretend.py:46::initialise");
    let root = snapshot(rows).root;
    assert!(shelf_from_root(&root, KeyTag::from_key(root.root().as_bytes())).is_empty());
    let package = Row::new(RowId::Package(package_key(PROJECT)), basis(), PROJECT);
    let mut rows = root.rows().to_vec();
    rows.push(package);
    let root = snapshot(rows).root;
    let shelf = shelf_from_root(&root, KeyTag::from_key(root.root().as_bytes()));
    assert_eq!(shelf.entries().len(), 1);
    assert!(shelf.entries()[0].languages().is_empty());
}

#[test]
fn foreign_page_member_relation_and_outline_lowering_preserve_absent_source_and_native_ids() {
    let rows = foreign_rows();
    let selected = symbol(&rows[0]);
    let mut child = rows[1].clone().with_parent(selected);
    child.label = "/abs/fake::src/not-captured.py:20::child".to_owned();
    let document = Document::new(
        selected,
        basis().root,
        [Fragment::Text("native docs".to_owned())],
    );
    let page = page_from_document_with_graph_relations(
        &rows[0].label,
        &document,
        &[rows[0].clone(), child.clone()],
        std::slice::from_ref(&child),
        &[GraphRelation::new(
            rows[0].id,
            child.id,
            SemanticLinkKind::Calls,
        )],
        Vec::new(),
    );
    assert_unattached(page.identity(), &rows[0]);
    assert_eq!(page.kind(), None);
    assert_eq!(
        page.source()
            .fault()
            .expect("absent typed source")
            .cause()
            .slug(),
        CauseSlug::NotCaptured
    );
    assert_unattached(page.members()[0].members()[0].identity(), &child);
    assert_unattached(page.relations()[0].relations()[0].identity(), &child);
    let dto = PageDto::new(&page);
    assert!(dto.source.is_none());
    assert_eq!(
        dto.source_fault.expect("source refusal").cause,
        "not-captured"
    );
    let mut resolver = row_resolver(&rows);
    assert_unattached(&resolver(selected).expect("exact native row").0, &rows[0]);
    for rendered in [text::page(&page, Theme::plain()), markdown::page(&page)] {
        assert!(rendered.contains("initialise"));
        assert!(rendered.contains("source-unavailable"));
    }
}

fn snapshot(rows: Vec<Row>) -> ViewSnapshot {
    let basis = basis();
    let root = ViewRoot::new_incomplete(
        view_key(b"typed-row-control"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
        rows,
        vec![],
    )
    .expect("incomplete control does not claim producer publication");
    ViewSnapshot {
        root,
        freshness: Freshness::Current,
        next: None,
        graph_relations: None,
        rich_graph: None,
    }
}

struct PageEngine {
    document: Document,
    related: ViewSnapshot,
    outline: ViewSnapshot,
    reads: Vec<String>,
}

impl Engine for PageEngine {
    fn revision(&mut self) -> Result<ViewStateRoot, ClientError> {
        Ok(basis().root)
    }
    fn health(&mut self) -> Result<HealthReport, ClientError> {
        Err(ClientError::Protocol(
            "health outside this control".to_owned(),
        ))
    }
    fn surface(&mut self, _: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
        Err(ClientError::Protocol(
            "mutation outside this control".to_owned(),
        ))
    }
    fn probe(&mut self, probe: Probe<'_>) -> Result<ReplyDto, ClientError> {
        let reply = match probe {
            Probe::Document(at) => {
                self.reads.push(format!("document:{at}"));
                CommandReply::Document(self.document.clone())
            }
            Probe::Related(at) => {
                self.reads.push(format!("related:{at}"));
                CommandReply::Graph(self.related.clone())
            }
            Probe::OutlinePage { path, .. } => {
                self.reads.push(format!("outline:{path}"));
                CommandReply::Names(self.outline.clone())
            }
            _ => return Err(ClientError::Protocol("unexpected read".to_owned())),
        };
        Ok(ReplyDto::new(1, reply))
    }
}

#[test]
fn shared_page_driver_fetches_an_outline_only_for_the_exact_selected_local_package_row() {
    let foreign = foreign_rows()[0].clone();
    let local = Row::in_package(
        RowId::Symbol(symbol_key("native-local")),
        basis(),
        package_key(PROJECT),
        format!("{PROJECT}::src/local.py:2::local"),
    )
    .with_kind(backend_library::DeclarationKind::Function)
    .with_source(SourceLocation::new("src/local.py", 2).expect("site"));
    let delimiter_root = "/abs/a::b";
    let delimiter_local = Row::in_package(
        RowId::Symbol(symbol_key("native-delimiter-local")),
        basis(),
        package_key(delimiter_root),
        format!("{delimiter_root}::src/local.py:3::local"),
    )
    .with_kind(backend_library::DeclarationKind::Function)
    .with_source(SourceLocation::new("src/local.py", 3).expect("site"));
    let identity = Identity::from_row(&delimiter_local);
    assert_eq!(
        identity.project().map(ProjectRef::root),
        Some(delimiter_root)
    );
    assert_eq!(identity.name(), "local");
    assert_eq!(
        identity.path().map(PackagePath::as_str),
        Some("src/local.py")
    );
    assert_eq!(identity.line().map(LineNumber::get), Some(3));
    let mut same_label_other_key = local.clone();
    same_label_other_key.label.clone_from(&foreign.label);
    for (selected, related, expected_outline) in [
        (foreign.clone(), vec![foreign.clone()], None),
        (local.clone(), vec![local.clone()], Some(PROJECT)),
        (
            delimiter_local.clone(),
            vec![delimiter_local.clone()],
            Some(delimiter_root),
        ),
        (foreign.clone(), vec![same_label_other_key], None),
        (local.clone(), vec![], None),
    ] {
        let mut document = Document::new(symbol(&selected), basis().root, [])
            .with_location(selected.source.clone());
        if selected.source.captured().is_some() {
            document = document.with_excerpt(
                SourceExcerpt::captured("local()", SourceExcerptExtent::Complete).expect("excerpt"),
            );
        }
        let mut engine = PageEngine {
            document,
            related: snapshot(related),
            outline: snapshot(vec![selected.clone()]),
            reads: Vec::new(),
        };
        let answer = answer(&mut engine, &Request::Page(selected.label.clone())).expect("page");
        let Answer::Page(page) = answer else {
            panic!("page reply")
        };
        assert_eq!(
            page.identity().key(),
            IdentityKey::Symbol(symbol(&selected))
        );
        assert_eq!(
            engine.reads,
            std::iter::once(format!("document:{}", selected.label))
                .chain(std::iter::once(format!("related:{}", selected.label)))
                .chain(expected_outline.map(|root| format!("outline:{root}")))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn native_foreign_rows_keep_exact_selected_ids_and_order_through_actual_library_pages() {
    use backend_library::{Cursor, Library, Query, QueryLimit};
    let root = snapshot(foreign_rows().to_vec()).root;
    let ids = root.rows().iter().map(|row| row.id).collect::<Vec<_>>();
    let before = root.clone();
    let library = Library::from_view(root.clone(), Cursor::for_view_root(&root)).expect("owner");
    let mut query = Query::new(
        "initialise",
        root.root(),
        QueryLimit::new(1).expect("credit"),
    );
    let mut observed = Vec::new();
    for expected in &ids {
        let page = library
            .search_from_ranked_ids(&query, &ids)
            .expect("actual owner page");
        let list = record_list("initialise", &page);
        assert_eq!(list.records().len(), 1);
        let row = root.row(*expected).expect("native endpoint");
        assert_unattached(list.records()[0].identity(), row);
        observed.push(list.records()[0].identity().key());
        if let Some(cursor) = page.next {
            query = query.with_cursor(cursor);
        }
    }
    assert_eq!(
        observed,
        ids.into_iter().map(IdentityKey::from).collect::<Vec<_>>()
    );
    assert_eq!(
        library.view(),
        &before,
        "paging and projection conserve the exact view"
    );
}

#[test]
fn opaque_object_rows_never_become_packages_or_selected_symbol_operands() {
    let row = Row::new(
        RowId::Object(object_version(b"native-object")),
        basis(),
        "/abs/fake::src/not-a-source.py:9::object",
    );
    let identity = Identity::from_row(&row);
    assert_eq!(identity.shape(), IdentityShape::Opaque);
    assert_eq!(identity.coordinate().as_str(), row.label);
    assert_eq!(identity.name(), row.label);
    assert_eq!(identity.key(), IdentityKey::Absent);
    assert_eq!(identity.project(), None);
    assert_eq!(identity.path(), None);
    assert_eq!(IdentityDto::new(&identity).semantic_data, None);
}

#[test]
fn absent_or_stale_source_facts_never_retain_a_display_line_or_invent_a_site() {
    let label = format!("{PROJECT}::src/display.py:999::local");
    let mut row = Row::in_package(
        RowId::Symbol(symbol_key("native-source-availability")),
        basis(),
        package_key(PROJECT),
        &label,
    );
    for availability in [
        SourceAvailability::NotCaptured,
        SourceAvailability::NotHydrated,
        SourceAvailability::Unconfigured,
        SourceAvailability::stale_file("src/actual.py").expect("stale typed path"),
    ] {
        row.source = availability.clone();
        let identity = Identity::from_row(&row);
        assert_eq!(
            identity.path().map(PackagePath::as_str),
            availability.file_path()
        );
        assert_eq!(identity.line(), None);
        let document = Document::new(symbol(&row), basis().root, []).with_location(availability);
        let page = page_from_document(
            &label,
            &document,
            std::slice::from_ref(&row),
            &[],
            Vec::new(),
        );
        assert_eq!(page.source().site(), None);
        assert!(page.source().fault().is_some());
        assert_eq!(page.identity().key(), IdentityKey::Symbol(symbol(&row)));
    }
}
