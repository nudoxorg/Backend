//! Composite show must retain useful output and report failed required reads.
use super::*;
use backend_library::{
    DocumentQuery, HealthReport, Library, PageRequest, Row, RowId, SourceExcerpt, SourceLocation,
    SurfaceReply, symbol_key,
};
use backend_present::{Engine, Probe};

const COORDINATE: &str = "/abs/app::scheduler.ts:88::generateSchedule";

struct Reader {
    catalog: Library,
    related_fails: bool,
    members_fail: bool,
    transport_fails: bool,
    calls: Vec<&'static str>,
}

impl Reader {
    fn new(related_fails: bool, members_fail: bool, captured_source: bool) -> Self {
        let basis = root().basis();
        let package = package_key("/abs/app");
        let row = Row::in_package(
            RowId::Symbol(symbol_key(COORDINATE)),
            basis,
            package,
            COORDINATE,
        )
        .with_signature("function generateSchedule() {}")
        .with_document(vec![backend_library::Fragment::Text(
            "RETAINED_DOCUMENT".to_owned(),
        )])
        .with_source(SourceLocation::new("scheduler.ts", 88).expect("site"))
        .with_excerpt(if captured_source {
            SourceExcerpt::capture_bounded("function generateSchedule() {}")
        } else {
            SourceExcerpt::NotHydrated
        });
        let view = ViewRoot::new_incomplete(
            view_key(b"composite show"),
            basis,
            root().frontier(),
            vec![Row::new(RowId::Package(package), basis, "/abs/app"), row],
            Vec::new(),
        )
        .expect("incomplete fixture");
        let catalog =
            Library::from_view(view.clone(), backend_library::Cursor::for_view_root(&view))
                .expect("catalog");
        Self {
            catalog,
            related_fails,
            members_fail,
            transport_fails: false,
            calls: Vec::new(),
        }
    }
    fn snapshot(&self) -> ViewSnapshot {
        ViewSnapshot {
            root: self.catalog.view().clone(),
            freshness: Freshness::Current,
            next: None,
            graph_relations: None,
            rich_graph: None,
        }
    }
}

impl Engine for Reader {
    fn revision(&mut self) -> Result<backend_library::ViewStateRoot, ClientError> {
        Ok(self.catalog.revision_root())
    }
    fn health(&mut self) -> Result<HealthReport, ClientError> {
        panic!("show does not probe health")
    }
    fn surface(&mut self, _: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
        panic!("show does not mutate")
    }
    fn probe(&mut self, probe: Probe<'_>) -> Result<ReplyDto, ClientError> {
        match probe {
            Probe::Declaration { selector, read } => {
                use backend_library::{DeclarationRead, DocumentSelection};
                let symbol = selector.address().resolve(self.catalog.view()).ok_or(ClientError::CommandFailed(backend_library::CommandFailure::NotFound))?;
                let row = self.catalog.view().row(RowId::Symbol(symbol)).ok_or(ClientError::CommandFailed(backend_library::CommandFailure::NotFound))?;
                if selector.coordinate().is_some_and(|label| label != row.label) {
                    return Err(ClientError::Protocol("selected coordinate mismatch".to_owned()));
                }
                match read {
                    DeclarationRead::Document | DeclarationRead::Source => {
                        self.calls.push("selected document");
                        let document = self.catalog.document(DocumentQuery::selected(symbol, self.catalog.revision_root())).expect("selected current row");
                        let selection = DocumentSelection::from_row(&row, document.source_basis().expect("source basis")).expect("selected label");
                        return Ok(ReplyDto::new(1, CommandReply::Document(document.with_selection(selection).expect("exact selected row"))));
                    }
                    DeclarationRead::Related | DeclarationRead::Graph => {
                        self.calls.push("selected related");
                        return Ok(ReplyDto::new(2, CommandReply::Graph(self.snapshot())));
                    }
                }
            }
            Probe::Document(COORDINATE) | Probe::Source(COORDINATE) => {
                self.calls.push("document");
                Ok(ReplyDto::new(
                    1,
                    CommandReply::Document(
                        self.catalog
                            .document(DocumentQuery::selected(
                                symbol_key(COORDINATE),
                                self.catalog.revision_root(),
                            ))
                            .expect("document"),
                    ),
                ))
            }
            Probe::Related(COORDINATE) => {
                self.calls.push("related");
                if self.transport_fails {
                    Err(ClientError::Disconnected(
                        std::io::ErrorKind::ConnectionReset,
                    ))
                } else if self.related_fails {
                    Err(ClientError::Protocol("RELATED_PROOF_REFUSED".to_owned()))
                } else {
                    Ok(ReplyDto::new(2, CommandReply::Graph(self.snapshot())))
                }
            }
            Probe::OutlinePage { path, limit } if path == "/abs/app" => {
                self.calls.push("members");
                if self.members_fail {
                    Err(ClientError::Protocol(
                        "DigestMismatch MEMBER_PROOF_REFUSED".to_owned(),
                    ))
                } else {
                    let page = self
                        .catalog
                        .outline_page(
                            package_key(path),
                            PageRequest::new(
                                self.catalog.revision_root(),
                                QueryLimit::new(limit).expect("admitted outline credit"),
                            ),
                        )
                        .expect("actual revision-bound member projection");
                    Ok(ReplyDto::new(3, CommandReply::ProjectionPage(page)))
                }
            }
            _ => panic!("unexpected probe {probe:?}"),
        }
    }
}

#[test]
fn show_required_subread_failure_preserves_output_and_returns_nonzero() {
    for (related, members, expected) in [
        (false, false, None),
        (false, true, Some("MEMBER_PROOF_REFUSED")),
        (true, false, Some("RELATED_PROOF_REFUSED")),
        (true, true, Some("RELATED_PROOF_REFUSED")),
    ] {
        let mut reader = Reader::new(related, members, true);
        let answer = backend_present::answer(&mut reader, &Request::Page(COORDINATE.to_owned()))
            .expect("useful admitted page");
        assert_eq!(reader.calls, ["document", "related", "members"]);
        assert_eq!(
            render::answer_exit_code(&answer),
            if expected.is_some() {
                ExitCode::from(render::EXIT_REFUSED)
            } else {
                ExitCode::SUCCESS
            }
        );
        if let Some(marker) = expected {
            let fault = answer.fault().expect("hard subread failure");
            assert_eq!(fault.slug(), FaultSlug::Protocol);
            assert!(
                fault.cause().sentence().contains(marker),
                "first required probe failure wins deterministically"
            );
        } else {
            assert!(answer.fault().is_none());
        }
        for format in ["human", "markdown", "json"] {
            let (options, _) = options::split(&words(&format!(
                "--no-color --format {format} --detail full show {COORDINATE}"
            )))
            .expect("options");
            let output =
                render::try_answer_with_cursor(&answer, &options, None).expect("complete output");
            assert!(output.contains("RETAINED_DOCUMENT"));
            assert!(output.contains(COORDINATE));
            if let Some(marker) = expected {
                assert!(output.contains(marker));
            }
            if related && members {
                assert!(
                    output.contains("MEMBER_PROOF_REFUSED"),
                    "secondary faults remain visible"
                );
            }
            if format == "json" {
                let value: serde_json::Value = serde_json::from_str(&output).expect("typed JSON");
                assert_eq!(value["answer"], "page");
                assert_eq!(
                    value
                        .get("notes")
                        .and_then(serde_json::Value::as_array)
                        .map_or(0, Vec::len),
                    usize::from(related) + usize::from(members)
                );
                if members {
                    assert!(
                        value["notes"]
                            .as_array()
                            .expect("notes")
                            .iter()
                            .any(|note| note["operand"] == "/abs/app"
                                && note["call"]["arguments"]["path"] == "/abs/app")
                    );
                }
            }
        }
    }
}

#[test]
fn show_optional_source_absence_is_success_but_source_request_is_not() {
    let mut reader = Reader::new(false, false, false);
    let answer =
        backend_present::answer(&mut reader, &Request::Page(COORDINATE.to_owned())).expect("page");
    assert_eq!(render::answer_exit_code(&answer), ExitCode::SUCCESS);
    let value: serde_json::Value = serde_json::from_str(&render::json(&answer)).expect("JSON");
    assert_eq!(value["source_fault"]["slug"], "source-unavailable");
    let fault = backend_present::answer(&mut reader, &Request::Source(COORDINATE.to_owned()))
        .expect_err("source is required by the source command");
    assert_eq!(fault.slug(), FaultSlug::SourceUnavailable);
    assert_eq!(
        render::exit_code(&fault),
        ExitCode::from(render::EXIT_REFUSED)
    );
}

#[test]
fn show_transport_failure_keeps_admitted_document_and_io_exit() {
    let mut reader = Reader::new(false, false, true);
    reader.transport_fails = true;
    let answer = backend_present::answer(&mut reader, &Request::Page(COORDINATE.to_owned()))
        .expect("document survives disconnected related read");
    let expected = backend_present::Fault::from_client_error(
        &ClientError::Disconnected(std::io::ErrorKind::ConnectionReset),
        backend_present::Operand::Coordinate(backend_present::Coordinate::new(COORDINATE)),
    );
    assert_eq!(
        answer.fault().expect("transport failure").slug(),
        expected.slug()
    );
    assert_eq!(
        render::answer_exit_code(&answer),
        render::exit_code(&expected)
    );
    assert_ne!(render::answer_exit_code(&answer), ExitCode::SUCCESS);
    let output = render::json_with_detail(&answer, backend_present::Detail::Full);
    assert!(output.contains("RETAINED_DOCUMENT"));
    assert_eq!(reader.calls, ["document", "related", "members"]);
}

#[test]
fn show_optional_scope_notices_do_not_mask_first_required_failure() {
    use backend_present::{Affordance, Cause, CauseSlug, Fault, Operand};
    let mut reader = Reader::new(false, false, false);
    let Answer::Page(page) =
        backend_present::answer(&mut reader, &Request::Page(COORDINATE.to_owned())).expect("page")
    else {
        panic!("page")
    };
    let notices = [
        FaultSlug::LaneUnavailable,
        FaultSlug::LanePartial,
        FaultSlug::PartiallyPublished,
        FaultSlug::SourceUnavailable,
    ]
    .map(|slug| {
        Fault::new(
            slug,
            Operand::Whole,
            Cause::new(CauseSlug::Unproven, "OPTIONAL_SCOPE"),
            Affordance::None,
        )
    });
    let page = (*page).with_notes(Vec::from(notices));
    assert_eq!(
        render::answer_exit_code(&Answer::Page(Box::new(page.clone()))),
        ExitCode::SUCCESS
    );
    let first = Fault::from_client_error(
        &ClientError::Protocol("FIRST_HARD_FAILURE".to_owned()),
        Operand::Path("/abs/app".to_owned()),
    );
    let later = Fault::from_client_error(
        &ClientError::Io("LATER_IO_FAILURE".to_owned()),
        Operand::Whole,
    );
    let mut notes = page.notes().to_vec();
    notes.extend([first.clone(), later]);
    let answer = Answer::Page(Box::new(page.with_notes(notes)));
    assert_eq!(
        answer.fault(),
        Some(&first),
        "first hard section failure retains its exact typed identity"
    );
    assert_eq!(
        render::answer_exit_code(&answer),
        ExitCode::from(render::EXIT_REFUSED)
    );
    let output = render::json(&answer);
    assert!(output.contains("FIRST_HARD_FAILURE"));
    assert!(output.contains("LATER_IO_FAILURE"));
    assert!(output.contains("OPTIONAL_SCOPE"));
}

#[test]
fn large_show_defaults_keep_exact_identity_and_page_all_members_with_zero_exit() {
    let mut reader = Reader::new(false, false, true);
    let view = reader.catalog.view();
    let basis = view.basis();
    let package = package_key("/abs/app");
    let mut rows = view.rows().to_vec();
    for index in 0..80 {
        let label = format!("{COORDINATE}::member_{index:03}");
        rows.push(Row::in_package(RowId::Symbol(symbol_key(&label)), basis, package, &label)
            .with_parent(symbol_key(COORDINATE))
            .with_kind(backend_library::DeclarationKind::Method)
            .with_signature(format!("def member_{index:03}(self, document: Document) -> Document:")));
    }
    let view = backend_library::ViewRoot::new_incomplete(
        view.recipe(), basis, view.frontier(), rows, Vec::new()).expect("expanded fixture view");
    reader.catalog = Library::from_view(view.clone(), backend_library::Cursor::for_view_root(&view)).expect("catalog");
    let mut request = Request::Page(COORDINATE.to_owned());
    let (options, _) = options::split(&words(&format!("--json show {COORDINATE}"))).expect("useful default");
    let mut seen = std::collections::BTreeSet::new();
    loop {
        let answer = backend_present::answer(&mut reader, &request).expect("shared driver");
        assert_eq!(render::answer_exit_code(&answer), ExitCode::SUCCESS);
        let output = render::try_answer_with_cursor(&answer, &options, None).expect("ordinary default response fits");
        let value: serde_json::Value = serde_json::from_str(&output).expect("JSON");
        assert_eq!(value["identity"]["coordinate"], COORDINATE);
        assert_eq!(value["identity"]["semantic_data"]["value"], serde_json::json!(symbol_key(COORDINATE).as_bytes()));
        assert!(value["signature"].as_str().expect("useful signature").contains("generateSchedule"));
        for group in value["members"].as_array().expect("member groups") {
            for row in group["members"].as_array().expect("rows") {
                assert!(seen.insert(row["identity"]["coordinate"].as_str().expect("exact address").to_owned()));
            }
        }
        let Some(next) = value["section_page"]["continuation"].as_object() else { break; };
        let continuation: backend_present::DocumentContinuation = serde_json::from_value(serde_json::Value::Object(next.clone())).expect("complete selected continuation");
        assert_eq!(continuation.selected_symbol, backend_present::IdentitySemanticData::SelectedSymbolId(backend_library::SelectedSymbolId::new(*symbol_key(COORDINATE).as_bytes()).expect("selected id")));
        let invocation = invoke::parse(&continuation.cli_arguments(), None).expect("copied CLI continuation grammar");
        request = backend_present::lower(&invocation, "/abs/app").expect("shared lowering");
    }
    assert_eq!(seen.len(), 80, "no member dropped or repeated");
    assert_eq!(reader.calls.iter().filter(|call| **call == "document").count(), 1,
        "only the initial legacy call used coordinate lookup");
}

#[test]
fn copied_show_selected_continuation_keeps_same_coordinate_rows_distinct() {
    use backend_library::{DeclarationRead, DeclarationSelector, SelectedSymbolId};
    let mut reader = Reader::new(false, false, true);
    let basis = reader.catalog.view().basis();
    let mut rows = reader.catalog.view().rows().to_vec();
    let other = symbol_key("second same-coordinate declaration");
    rows.push(Row::in_package(RowId::Symbol(other), basis, package_key("/abs/app"), COORDINATE)
        .with_kind(backend_library::DeclarationKind::Class).with_signature("class Second:"));
    for (owner, family) in [(symbol_key(COORDINATE), "first"), (other, "second")] {
        for index in 0..24 {
            let label = format!("/abs/app::scheduler.ts:{}::{family}_{index:02}", index + 100);
            rows.push(Row::in_package(RowId::Symbol(symbol_key(&label)), basis, package_key("/abs/app"), &label)
                .with_parent(owner).with_kind(backend_library::DeclarationKind::Method));
        }
    }
    let view = backend_library::ViewRoot::new_incomplete(reader.catalog.view().recipe(), basis, reader.catalog.view().frontier(), rows, Vec::new()).expect("two same-coordinate roots");
    reader.catalog = Library::from_view(view.clone(), backend_library::Cursor::for_view_root(&view)).expect("catalog");
    for (native, other) in [(symbol_key(COORDINATE), other), (other, symbol_key(COORDINATE))] {
        reader.calls.clear();
        let request = Request::Declaration { selector: DeclarationSelector::SelectedAt { symbol: SelectedSymbolId::new(*native.as_bytes()).expect("full selected id"), coordinate: COORDINATE.to_owned() }, read: DeclarationRead::Document };
        let first = backend_present::answer(&mut reader, &request).expect("selected first page");
        let Answer::Page(page) = first else { panic!("page"); };
        let next = page.section_page().expect("extent").continuation.as_ref().expect("next selected page");
        let invocation = invoke::parse(&next.cli_arguments(), None).expect("copy exact CLI continuation");
        let resumed = backend_present::lower(&invocation, "/abs/app").expect("typed selected continuation");
        let answer = backend_present::answer(&mut reader, &resumed).expect("same selected row");
        let Answer::Page(page) = answer else { panic!("page"); };
        assert_eq!(page.identity().key(), backend_present::IdentityKey::Symbol(native));
        let mut wrong = next.clone();
        wrong.selected_symbol = backend_present::IdentitySemanticData::SelectedSymbolId(SelectedSymbolId::new(*other.as_bytes()).expect("other full id"));
        let input = invoke::parse(&wrong.cli_arguments(), None).expect("syntax still valid");
        let input = backend_present::lower(&input, "/abs/app").expect("typed changed selector");
        assert!(backend_present::answer(&mut reader, &input).is_err(), "same display coordinate cannot substitute another selected row");
        assert!(!reader.calls.contains(&"document"), "selected reads never fall back to coordinate lookup");
    }
}
