#![allow(clippy::expect_used)]

use super::*;
use backend_library::{
    AuthorityScopeClaim, Basis, Coverage, CoverageCapability, Cursor, Frontier, Library,
    OutlineCursorWitness, ProducerObservationClaims, ProducerObservationVerifier,
    QueryPageRecipe, Row, RowId, ScopeRoot, UntrustedProducerObservation, ViewRoot,
    admit_complete_scope, admit_producer_observation,
    object_version, symbol_key, view_key, view_state_root, view_version_preimage,
};
use std::sync::{Arc, Mutex};

struct Verifier(u8);
impl ProducerObservationVerifier for Verifier {
    type Error = &'static str;
    fn verify(
        &self,
        observation: &UntrustedProducerObservation,
    ) -> Result<ProducerObservationClaims, Self::Error> {
        if observation.producer_identity() != [11; 32]
            || observation.context() != [self.0; 32]
            || observation.evidence() != [self.0; 32]
        {
            return Err("foreign test authority");
        }
        Ok(ProducerObservationClaims::new(
            observation.producer_identity(),
            observation.scope_root(),
            observation.context(),
            *blake3::hash(observation.evidence()).as_bytes(),
        ))
    }
}

fn owner(context: u8, count: usize, sequence: u64) -> Library {
    owner_with_label(context, count, sequence, "Thing repeated")
}
fn owner_with_label(context: u8, count: usize, sequence: u64, label: &str) -> Library {
    owner_with_labels(context, count, sequence, &[label])
}
fn owner_with_labels(context: u8, count: usize, sequence: u64, labels: &[&str]) -> Library {
    owner_with_package(context, count, sequence, labels, None)
}
fn owner_with_package(
    context: u8,
    count: usize,
    sequence: u64,
    labels: &[&str],
    package: Option<&str>,
) -> Library {
    let object = object_version(b"library-source-v1");
    let scope = ScopeRoot::from_bytes(object.to_bytes());
    let observation = admit_producer_observation(
        UntrustedProducerObservation::new([11; 32], scope, [context; 32], vec![context; 32]),
        &Verifier(context),
    )
    .expect("owned test authority");
    let capability = CoverageCapability::from_authorized_with_evidence(
        admit_complete_scope(
            AuthorityScopeClaim::from_object_version(object),
            observation,
        )
        .expect("scope"),
        vec![context; 32],
    )
    .expect("evidence");
    let basis = Basis::new(view_state_root(&[]), object);
    let mut rows: Vec<Row> = labels
        .iter()
        .cycle()
        .take(count)
        .enumerate()
        .map(|(i, label)| {
            Row::new(
                RowId::Symbol(symbol_key(&format!("pkg::Thing{i:03}"))),
                basis,
                *label,
            )
        })
        .collect();
    if let Some(package) = package {
        let key = backend_library::package_key(package);
        for row in &mut rows {
            row.package = Some(key);
        }
        rows.push(Row::new(RowId::Package(key), basis, package));
    }
    let root = ViewRoot::new_checked(
        view_key(b"library-view-v1"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
        rows,
        vec![Coverage::Complete],
        capability,
    )
    .expect("owner root");
    Library::from_view(root.clone(), Cursor::for_view_root_at(&root, sequence))
        .expect("owner library")
}

struct Transport {
    owner: Arc<Mutex<Library>>,
    requests: Arc<Mutex<Vec<Command>>>,
    graph_ids: Option<(SymbolKey, Vec<RowId>)>,
}
impl CommandTransport for Transport {
    fn request(&mut self, request: CommandDto) -> Result<ReplyDto, ClientError> {
        self.requests
            .lock()
            .expect("requests")
            .push(request.command.clone());
        let library = self.owner.lock().expect("owner");
        let is_catalog_continuation = match &request.command {
            Command::Search(query) => query.cursor().is_some(),
            Command::SearchPublishedNames(query) => query.query().cursor().is_some(),
            Command::Name(query) => query.cursor().is_some(),
            Command::GraphPage { page, .. } | Command::OutlinePage { page, .. } => {
                page.continuation().is_some()
            }
            _ => false,
        };
        let is_outline_witness = matches!(
            &request.command,
            Command::OutlinePage { page, .. } if page.outline_witness().is_some()
        );
        let request = if is_outline_witness {
            let bytes = backend_library::encode_command_body(&request)
                .map_err(ClientError::Protocol)?;
            let admitted = backend_library::decode_command_body_for_owner(
                &bytes,
                library.cursor(),
            )
            .map_err(ClientError::Protocol)?;
            assert_eq!(admitted, request, "explicit owner witness wire round-trip");
            let Command::OutlinePage { package, page } = &admitted.command else {
                panic!("outline witness command");
            };
            let witness = page.outline_witness().expect("untrusted witness");
            let certificate = admitted.certificate().expect("witness certificate");
            let recipe_id = backend_library::encode_id(witness.recipe().as_bytes());
            let mut recipes = certificate.claims.iter().filter_map(|claim| match claim {
                WireClaim::KeyBytes {
                    schema: WireSchema::ViewRecipe,
                    id,
                    value,
                } if id == &recipe_id => Some(value.as_ref()),
                _ => None,
            });
            let preimage = recipes.next().expect("recipe preimage");
            assert!(recipes.next().is_none(), "unique recipe preimage");
            let cursor = library
                .rehydrate_outline_cursor(
                    *package,
                    page.basis(),
                    page.window(),
                    witness,
                    preimage,
                )
                .map_err(|error| ClientError::Protocol(error.to_string()))?;
            CommandDto::new(
                admitted.request_id,
                Command::OutlinePage {
                    package: *package,
                    page: page.with_continuation(PageContinuation::from_cursor(cursor)),
                },
            )
            .with_certificate(certificate.clone())
        } else if is_catalog_continuation {
            let bytes =
                backend_library::encode_command_body(&request).map_err(ClientError::Protocol)?;
            let admitted = backend_library::decode_command_body_for_owner(&bytes, library.cursor())
                .map_err(ClientError::Protocol)?;
            assert_eq!(admitted, request, "normal encoder and owner decoder agree");
            admitted
        } else {
            request
        };
        if matches!(
            &request.command,
            Command::GraphPage { .. }
                | Command::OutlinePage { .. }
                | Command::MembersPage { .. }
                | Command::SearchPublishedNames(_)
        ) {
            if !is_catalog_continuation && !is_outline_witness {
                let bytes = backend_library::encode_command_body(&request)
                    .map_err(ClientError::Protocol)?;
                let admitted =
                    backend_library::decode_command_body(&bytes).map_err(ClientError::Protocol)?;
                assert_eq!(
                    admitted, request,
                    "catalog strict standalone command admission"
                );
            }
        }
        let mut reply = if let Command::SearchPublishedNames(query) = &request.command {
            let snapshot = library
                .search_published_names(query)
                .map_err(|error| ClientError::Protocol(error.to_string()))?;
            ReplyDto::new(
                request.request_id,
                CommandReply::PublishedNameSearch(backend_library::PublishedNameSearchPage {
                    snapshot,
                    readiness: backend_library::PublishedSearchReadiness {
                        full_search: backend_library::FullSearchReadiness::Preparing,
                        semantic: backend_library::PublishedSemanticReadiness::Pending,
                    },
                }),
            )
        } else if let Command::GraphQuery(query) = &request.command {
            let bytes =
                backend_library::encode_command_body(&request).map_err(ClientError::Protocol)?;
            let admitted = backend_library::decode_command_body_for_owner(&bytes, library.cursor())
                .map_err(ClientError::Protocol)?;
            assert_eq!(admitted, request, "actual strict owner command admission");
            let start = query
                .start_offset(library.cursor())
                .map_err(|error| ClientError::Protocol(error.to_string()))?;
            let count = library.view().row_refs().count();
            let end = (start + usize::from(query.page().limit().get())).min(count);
            let rows = (start..end)
                .map(|i| {
                    backend_library::GraphQueryRow::new(BTreeMap::from([(
                        "coordinate".to_owned(),
                        GraphValue::String(format!("Thing{i}")),
                    )]))
                    .expect("admitted projected row")
                })
                .collect::<Vec<_>>()
                .into_boxed_slice();
            let terminal = if end < count {
                PageTerminal::More(
                    query
                        .next_continuation(library.cursor(), end)
                        .expect("next"),
                )
            } else {
                PageTerminal::Complete
            };
            let (rows, terminal) = if query.control() == backend_library::GraphQueryControl::Cancel
            {
                (Box::new([]) as Box<[_]>, PageTerminal::Cancelled)
            } else {
                (rows, terminal)
            };
            ReplyDto::new(
                request.request_id,
                CommandReply::GraphQueryPage(GraphQueryPage {
                    revision: query.page().basis(),
                    source: library.view().basis().object,
                    rows,
                    terminal,
                }),
            )
            .with_certificate(request.certificate().expect("owner proof").clone())
        } else if let (Command::GraphPage { symbol, page }, Some((selected, ids))) =
            (&request.command, &self.graph_ids)
        {
            let reply = library
                .graph_page_from_ids(*selected, *symbol, *page, ids)
                .map(CommandReply::ProjectionPage)
                .unwrap_or_else(|error| CommandReply::Failed(error.into()));
            ReplyDto::new(request.request_id, reply)
        } else {
            library.execute_dto(request.clone())
        };
        let snapshot = match &reply.reply {
            CommandReply::Search(page) | CommandReply::Names(page) | CommandReply::Graph(page) => {
                Some(page)
            }
            CommandReply::ProjectionPage(page) => Some(&page.snapshot),
            CommandReply::PublishedNameSearch(page) => Some(&page.snapshot),
            _ => None,
        };
        if let Some(page) = snapshot {
            let source_root = library.revision_root();
            let recipe = match &request.command {
                Command::Search(query) => QueryPageRecipe::search(library.revision_root(), query)
                    .canonical_preimage()
                    .to_vec(),
                Command::SearchPublishedNames(query) => {
                    QueryPageRecipe::published_names(library.revision_root(), query)
                        .canonical_preimage()
                        .to_vec()
                }
                Command::Name(query) => QueryPageRecipe::names(library.revision_root(), query)
                    .canonical_preimage()
                    .to_vec(),
                Command::GraphPage { symbol, page } => {
                    QueryPageRecipe::graph_window(library.revision_root(), *symbol, page.window())
                        .canonical_preimage()
                        .to_vec()
                }
                Command::MembersPage { owner, page } => {
                    QueryPageRecipe::members(library.revision_root(), owner.symbol(), page.limit())
                        .canonical_preimage()
                        .to_vec()
                }
                Command::OutlinePage { package, page } => {
                    let CommandReply::ProjectionPage(projection) = &reply.reply else {
                        panic!("outline page");
                    };
                    QueryPageRecipe::outline_window(
                        library.revision_root(),
                        *package,
                        page.window(),
                        projection
                            .outline_order
                            .as_ref()
                            .expect("native outline order"),
                    )
                    .canonical_preimage()
                    .to_vec()
                }
                Command::Graph(_) | Command::Related(_) => {
                    let mut bytes = Vec::new();
                    for part in [b"query".as_slice(), b"graph", source_root.as_bytes(), &[0]] {
                        bytes.extend_from_slice(&(part.len() as u64).to_be_bytes());
                        bytes.extend_from_slice(part);
                    }
                    bytes
                }
                _ => unreachable!("query reply"),
            };
            let mut proof = library
                .execute_dto(CommandDto::new(1, Command::Revision))
                .certificate()
                .expect("revision certificate")
                .clone();
            if let Some(request_proof) = request.certificate() {
                for claim in &request_proof.claims {
                    if graph_address_claim(&request.command, claim)
                        || matches!(
                            claim,
                            WireClaim::Key {
                                schema: WireSchema::Symbol,
                                ..
                            }
                        )
                    {
                        proof = proof.with_claim_once(claim.clone());
                    }
                }
            }
            let root = &page.root;
            for claim in [
                WireClaim::KeyBytes {
                    schema: WireSchema::ViewRecipe,
                    id: encode_id(root.recipe().as_bytes()),
                    value: recipe.into_boxed_slice(),
                },
                WireClaim::Version {
                    schema: WireSchema::ViewVersion,
                    id: encode_id(root.version().as_bytes()),
                    value: view_version_preimage(
                        root.recipe(),
                        root.basis(),
                        root.frontier(),
                        root.root(),
                        root.coverage(),
                    )
                    .into_boxed_slice(),
                },
                WireClaim::Root {
                    schema: WireSchema::ViewRelation,
                    id: encode_id(root.root().as_bytes()),
                    canonical: root
                        .canonical_relation_bytes()
                        .expect("canonical predecessor")
                        .into_boxed_slice(),
                },
            ] {
                proof = proof.with_claim_once(claim);
            }
            for row in root.rows() {
                // The fixture acts as the producer here. Carry every typed
                // key and identity witness in the canonical snapshot rows,
                // including packages and parents outside this bounded page.
                let row_key = match row.id {
                    RowId::Package(package) => {
                        Some((WireSchema::Package, encode_id(package.as_bytes())))
                    }
                    RowId::Symbol(symbol) => {
                        Some((WireSchema::Symbol, encode_id(symbol.as_bytes())))
                    }
                    RowId::Object(_) => None,
                };
                if let Some((schema, id)) = row_key {
                    proof = proof.with_claim_once(match row.identity_preimage() {
                        Some(preimage) => WireClaim::RowIdentity {
                            schema,
                            id,
                            preimage: preimage.as_str().to_owned(),
                        },
                        None => WireClaim::KeyCommitment { schema, id },
                    });
                }
                if let Some(package) = row.package {
                    proof = proof.with_claim_once(WireClaim::KeyCommitment {
                        schema: WireSchema::Package,
                        id: encode_id(package.as_bytes()),
                    });
                }
                if let Some(parent) = row.parent {
                    proof = proof.with_claim_once(WireClaim::KeyCommitment {
                        schema: WireSchema::Symbol,
                        id: encode_id(parent.as_bytes()),
                    });
                }
            }
            reply = reply.with_certificate(proof);
        }
        if let CommandReply::Document(document) = &reply.reply {
            let row = library
                .view()
                .row_ref(RowId::Symbol(document.symbol))
                .expect("selected row");
            let source = Basis {
                root: library.revision_root(),
                ..library.view().basis()
            };
            let selection = backend_library::DocumentSelection::from_row(row, source)
                .expect("actual row selection");
            let mut proof = library
                .execute_dto(CommandDto::new(1, Command::Revision))
                .certificate()
                .expect("revision proof")
                .clone();
            for claim in request
                .certificate()
                .into_iter()
                .flat_map(|proof| &proof.claims)
            {
                proof = proof.with_claim_once(claim.clone());
            }
            proof = proof
                .with_claim_once(WireClaim::KeyCommitment {
                    schema: WireSchema::Symbol,
                    id: encode_id(document.symbol.as_bytes()),
                })
                .with_claim_once(WireClaim::Version {
                    schema: WireSchema::Document,
                    id: encode_id(selection.version().as_bytes()),
                    value: selection.canonical_bytes().into_boxed_slice(),
                });
            reply = ReplyDto::new(
                request.request_id,
                CommandReply::Document(
                    document
                        .clone()
                        .with_selection(selection)
                        .expect("actual document selection"),
                ),
            )
            .with_certificate(proof);
        }
        // Exercise the real strict producer/certificate and reply admission,
        // rather than returning caller-owned snapshots unchecked.
        if !matches!(
            &reply.reply,
            CommandReply::Failed(_) | CommandReply::Error(_)
        ) {
            let bytes = serde_json::to_vec(&reply).expect("producer wire");
            reply = ReplyDto::decode_with_certificate(&bytes, library.view().capability())
                .map_err(ClientError::Protocol)?;
        }
        admit_reply(&request, reply)
    }
}

fn session(owner: &Arc<Mutex<Library>>, requests: &Arc<Mutex<Vec<Command>>>) -> Session {
    Session::from_transport(
        "/private/test-owner.sock",
        Transport {
            owner: owner.clone(),
            requests: requests.clone(),
            graph_ids: None,
        },
    )
}
fn fixture() -> (Arc<Mutex<Library>>, Arc<Mutex<Vec<Command>>>) {
    (
        Arc::new(Mutex::new(owner(1, 37, 0))),
        Arc::new(Mutex::new(vec![])),
    )
}

fn selected_at(symbol: SymbolKey, coordinate: &str) -> DeclarationSelector {
    DeclarationSelector::SelectedAt {
        symbol: backend_library::SelectedSymbolId::new(symbol.to_bytes()).expect("selected row"),
        coordinate: coordinate.to_owned(),
    }
}

#[test]
fn selected_coordinate_reads_refuse_older_daemon_echoes_with_a_different_payload_label() {
    let owner = Arc::new(Mutex::new(owner_with_labels(
        1,
        2,
        0,
        &["actual label", "asserted label"],
    )));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let symbol = symbol_key("pkg::Thing000");
    for read in [
        DeclarationRead::Document,
        DeclarationRead::Source,
        DeclarationRead::Graph,
        DeclarationRead::Related,
    ] {
        let mut client = session(&owner, &requests);
        let correct = selected_at(symbol, "actual label");
        client
            .declaration(&correct, read)
            .expect("matching admitted payload");
        let wrong = selected_at(symbol, "asserted label");
        assert!(
            matches!(client.declaration(&wrong, read), Err(ClientError::Protocol(message))
            if message.contains("asserted coordinate")),
            "{read:?}"
        );
    }
    assert!(
        requests
            .lock()
            .expect("requests")
            .iter()
            .any(|command| matches!(command, Command::Graph(_)))
    );
}

struct MissingDocumentSelection(Transport);
impl CommandTransport for MissingDocumentSelection {
    fn request(&mut self, request: CommandDto) -> Result<ReplyDto, ClientError> {
        let mut reply = self.0.request(request.clone())?;
        if let CommandReply::Document(document) = &mut reply.reply {
            document.selection = None;
            let library = self.0.owner.lock().expect("owner");
            reply = ReplyDto::decode_with_certificate(
                &serde_json::to_vec(&reply).expect("legacy document without selection"),
                library.view().capability(),
            )
            .map_err(ClientError::Protocol)?;
            return admit_reply(&request, reply);
        }
        Ok(reply)
    }
}

#[test]
fn selected_coordinate_documents_refuse_a_legacy_payload_without_authenticated_selection() {
    let owner = Arc::new(Mutex::new(owner_with_label(1, 1, 0, "actual label")));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let mut client = Session::from_transport(
        "/private/test-owner.sock",
        MissingDocumentSelection(Transport {
            owner,
            requests,
            graph_ids: None,
        }),
    );
    let selected = selected_at(symbol_key("pkg::Thing000"), "actual label");
    for read in [DeclarationRead::Document, DeclarationRead::Source] {
        assert!(
            matches!(client.declaration(&selected, read), Err(ClientError::Protocol(message))
            if message.contains("asserted coordinate"))
        );
    }
}

fn selected_page_fixture() -> (
    Arc<Mutex<Library>>,
    Arc<Mutex<Vec<Command>>>,
    SymbolKey,
    Vec<RowId>,
) {
    let owner = Arc::new(Mutex::new(owner_with_label(1, 3, 0, "actual label")));
    let mut ids = owner
        .lock()
        .expect("owner")
        .view()
        .row_refs()
        .map(|row| row.id)
        .collect::<Vec<_>>();
    ids.sort_unstable();
    let RowId::Symbol(symbol) = *ids.last().expect("last center") else {
        panic!("symbol")
    };
    (owner, Arc::new(Mutex::new(Vec::new())), symbol, ids)
}

#[test]
fn selected_graph_pages_authenticate_an_omitted_center_and_keep_the_assertion_on_resume() {
    let (owner, requests, symbol, ids) = selected_page_fixture();
    let mut client = Session::from_transport(
        "/private/test-owner.sock",
        Transport {
            owner: owner.clone(),
            requests: requests.clone(),
            graph_ids: Some((symbol, ids.clone())),
        },
    );
    let selected = selected_at(symbol, "actual label");
    let window = backend_library::QueryWindow::fixed(QueryLimit::new(1).expect("one row"));
    let first = client
        .graph_page_declaration(&selected, window, None)
        .expect("small first page");
    let CommandReply::ProjectionPage(page) = first.reply else {
        panic!("graph page")
    };
    assert!(
        page.snapshot.root.row_ref(RowId::Symbol(symbol)).is_none(),
        "first page legitimately omits its center"
    );
    let PageTerminal::More(next) = page.terminal else {
        panic!("continued graph")
    };
    let token = client
        .encode_query_continuation(next)
        .expect("verified page has portable proof");
    let mut fresh = Session::from_transport(
        "/private/test-owner.sock",
        Transport {
            owner,
            requests: requests.clone(),
            graph_ids: Some((symbol, ids)),
        },
    );
    let continuation = fresh
        .decode_page_continuation(&token)
        .expect("same owner portable proof");
    fresh
        .graph_page_declaration(&selected, window, Some(continuation))
        .expect("continued assertion rechecked at exact source");
    let commands = requests.lock().expect("requests");
    let documents = commands
        .iter()
        .filter_map(|command| {
            if let Command::Document(query) = command {
                Some(query)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(documents.len(), 2);
    assert!(
        documents
            .iter()
            .all(|query| query.symbol() == selected.address() && query.source_basis().is_none())
    );
    assert_eq!(documents[0].basis(), documents[1].basis());
}

// Emulate the real local service's bounded revision proof, and cross the
// strict command codec for the center document as well as the graph request.
struct BoundedSelectedGraphTransport(Transport);
impl CommandTransport for BoundedSelectedGraphTransport {
    fn request(&mut self, request: CommandDto) -> Result<ReplyDto, ClientError> {
        if matches!(&request.command, Command::Document(_)) {
            let proof = request.certificate().expect("revision proof");
            assert!(proof.claims.iter().any(|claim| matches!(
                claim,
                WireClaim::RootCommitment {
                    schema: WireSchema::ViewRelation,
                    ..
                }
            )));
            assert!(
                !proof.claims.iter().any(|claim| matches!(
                    claim,
                    WireClaim::Root {
                        schema: WireSchema::ViewRelation,
                        ..
                    }
                )),
                "selected preflight must not ship the whole owner relation"
            );
            let bytes =
                backend_library::encode_command_body(&request).map_err(ClientError::Protocol)?;
            let admitted =
                backend_library::decode_command_body(&bytes).map_err(ClientError::Protocol)?;
            assert_eq!(admitted, request, "actual center request wire admission");
        }
        let revision = matches!(&request.command, Command::Revision);
        let mut reply = self.0.request(request)?;
        if revision {
            let root = self.0.owner.lock().expect("owner").revision_root();
            let certificate = reply.certificate().expect("owner revision proof");
            let claims = certificate
                .claims
                .iter()
                .filter(|claim| {
                    !matches!(
                        claim,
                        WireClaim::Root {
                            schema: WireSchema::ViewRelation,
                            ..
                        } | WireClaim::RootCommitment {
                            schema: WireSchema::ViewRelation,
                            ..
                        }
                    )
                })
                .cloned()
                .collect::<Vec<_>>();
            let mut proof = WireCertificate::new();
            for claim in claims {
                proof = proof.with_claim(claim);
            }
            reply = reply.with_certificate(proof.with_claim(WireClaim::RootCommitment {
                schema: WireSchema::ViewRelation,
                id: encode_id(root.as_bytes()),
            }));
            let bytes = serde_json::to_vec(&reply).expect("bounded owner revision wire");
            let library = self.0.owner.lock().expect("owner");
            reply = ReplyDto::decode_with_certificate(&bytes, library.view().capability())
                .map_err(ClientError::Protocol)?;
        }
        Ok(reply)
    }
}

#[test]
fn selected_graph_preflight_roundtrips_with_only_the_bounded_owner_commitment() {
    let (owner, requests, symbol, ids) = selected_page_fixture();
    let mut client = Session::from_transport(
        "/private/test-owner.sock",
        BoundedSelectedGraphTransport(Transport {
            owner,
            requests: requests.clone(),
            graph_ids: Some((symbol, ids.clone())),
        }),
    );
    let selected = selected_at(symbol, "actual label");
    let window = backend_library::QueryWindow::fixed(QueryLimit::new(1).expect("one row"));
    let mut next = None;
    let mut seen = Vec::new();
    loop {
        let reply = client
            .graph_page_declaration(&selected, window, next)
            .expect("bounded selected graph request and authenticated reply");
        let CommandReply::ProjectionPage(page) = reply.reply else {
            panic!("graph page")
        };
        seen.extend(page.snapshot.root.row_refs().map(|row| row.id));
        match page.terminal {
            PageTerminal::More(cursor) => next = Some(cursor),
            PageTerminal::Complete => break,
            PageTerminal::Cancelled => panic!("not cancelled"),
        }
    }
    assert_eq!(seen, ids);
    assert_eq!(
        requests
            .lock()
            .expect("requests")
            .iter()
            .filter(|command| matches!(command, Command::Document(_)))
            .count(),
        1
    );
}

#[test]
fn selected_graph_center_still_requires_every_exact_source_basis_component() {
    let (owner, _, symbol, _) = selected_page_fixture();
    let library = owner.lock().expect("owner");
    let source = Basis {
        root: library.revision_root(),
        ..library.view().basis()
    };
    let row = library
        .view()
        .row_ref(RowId::Symbol(symbol))
        .expect("center");
    let assertion = SelectedCoordinateAssertion {
        symbol: SymbolAddress::selected(symbol),
        coordinate: "actual label".to_owned(),
    };
    let reply = |basis: Basis| {
        let selection = backend_library::DocumentSelection::from_row(row, basis)
            .expect("independently consistent selection");
        ReplyDto::new(
            1,
            CommandReply::Document(
                backend_library::Document::new(symbol, basis.root, vec![])
                    .with_source_basis(basis)
                    .with_selection(selection)
                    .expect("consistent document"),
            ),
        )
    };
    assert!(assertion.admit_document(&reply(source), source).is_ok());
    let mut mutations = [source; 5];
    mutations[0].root = view_state_root(&[]);
    mutations[1].object = object_version(b"different captured source");
    mutations[2].branch = backend_library::branch_key("different branch");
    mutations[3].log = backend_library::log_key("different log");
    mutations[4].schema = source.schema + 1;
    for changed in mutations {
        assert_ne!(changed, source);
        assert!(
            matches!(
                assertion.admit_document(&reply(changed), source),
                Err(ClientError::StaleCursor)
            ),
            "changed source must not authenticate: {changed:?}"
        );
    }
}

#[test]
fn selected_graph_same_session_pages_reuse_only_the_exact_bounded_center_proof() {
    let (owner, requests, symbol, ids) = selected_page_fixture();
    let mut client = Session::from_transport(
        "/private/test-owner.sock",
        Transport {
            owner,
            requests: requests.clone(),
            graph_ids: Some((symbol, ids.clone())),
        },
    );
    let selected = selected_at(symbol, "actual label");
    let window = backend_library::QueryWindow::fixed(QueryLimit::new(1).expect("one row"));
    let mut continuation = None;
    let mut seen = Vec::new();
    loop {
        let reply = client
            .graph_page_declaration(&selected, window, continuation)
            .expect("same-session page");
        let CommandReply::ProjectionPage(page) = reply.reply else {
            panic!("graph page")
        };
        seen.extend(page.snapshot.root.row_refs().map(|row| row.id));
        assert!(
            client.continuations.len() <= 1,
            "proof uses only the existing bounded continuation slot"
        );
        match page.terminal {
            PageTerminal::More(next) => continuation = Some(next),
            PageTerminal::Complete => break,
            PageTerminal::Cancelled => panic!("not cancelled"),
        }
    }
    assert_eq!(seen, ids);
    let count_documents = || {
        requests
            .lock()
            .expect("requests")
            .iter()
            .filter(|command| matches!(command, Command::Document(_)))
            .count()
    };
    assert_eq!(
        count_documents(),
        1,
        "three credit1 pages authenticate the center once"
    );
    client
        .graph_page_declaration(&selected, window, None)
        .expect("new independent first page");
    assert_eq!(
        count_documents(),
        2,
        "a new query cannot borrow the predecessor's proof"
    );
}

#[test]
fn selected_graph_pages_refuse_a_rehashed_coordinate_claim_before_sending_or_exporting_a_page() {
    let (owner, requests, symbol, ids) = selected_page_fixture();
    let mut client = Session::from_transport(
        "/private/test-owner.sock",
        Transport {
            owner: owner.clone(),
            requests: requests.clone(),
            graph_ids: Some((symbol, ids.clone())),
        },
    );
    let window = backend_library::QueryWindow::fixed(QueryLimit::new(1).expect("one row"));
    let wrong = selected_at(symbol, "another label");
    assert!(client.graph_page_declaration(&wrong, window, None).is_err());
    assert!(
        client.continuations.is_empty(),
        "no cursor is retained before center authentication"
    );
    assert!(
        !requests
            .lock()
            .expect("requests")
            .iter()
            .any(|command| matches!(command, Command::GraphPage { .. }))
    );
    let first = client
        .graph_page_declaration(&selected_at(symbol, "actual label"), window, None)
        .expect("matching first page");
    let CommandReply::ProjectionPage(page) = first.reply else {
        panic!("graph page")
    };
    let PageTerminal::More(next) = page.terminal else {
        panic!("continued graph")
    };
    let token = client
        .encode_query_continuation(next)
        .expect("issued token");
    let imported =
        backend_library::decode_command_body(&decode_portable_body(&token).expect("body"))
            .expect("canonical token");
    let mut certificate = imported.certificate().expect("certificate").clone();
    for claim in &mut certificate.claims {
        if let WireClaim::Key {
            schema: WireSchema::Symbol,
            id,
            value,
        } = claim
        {
            *id = encode_id(symbol_key("another label").as_bytes());
            *value = "another label".to_owned();
        }
    }
    let changed = CommandDto::new(1, imported.command).with_certificate(certificate);
    let changed = encode_portable_proof(
        &backend_library::encode_binary_certificate_command(&changed).expect("rehashable claim"),
        "pc4-",
    )
    .expect("bounded token");
    let mut fresh = Session::from_transport(
        "/private/test-owner.sock",
        Transport {
            owner,
            requests: requests.clone(),
            graph_ids: Some((symbol, ids)),
        },
    );
    let next = fresh
        .decode_page_continuation(&changed)
        .expect("cursor itself still has its genuine recipe");
    let before = requests
        .lock()
        .expect("requests")
        .iter()
        .filter(|command| matches!(command, Command::GraphPage { .. }))
        .count();
    assert!(
        fresh
            .graph_page_declaration(&wrong, window, Some(next))
            .is_err()
    );
    let after = requests
        .lock()
        .expect("requests")
        .iter()
        .filter(|command| matches!(command, Command::GraphPage { .. }))
        .count();
    assert_eq!(
        before, after,
        "changed assertion cannot reuse a page that omits its center"
    );
    assert_eq!(fresh.continuations.len(), 1);
    assert!(
        fresh.continuations.contains_key(&next.cursor()),
        "only the imported predecessor remains; no successor was issued"
    );
}

struct RebindBeforeSelectedDocument(Transport);
impl CommandTransport for RebindBeforeSelectedDocument {
    fn request(&mut self, request: CommandDto) -> Result<ReplyDto, ClientError> {
        if matches!(&request.command, Command::Document(_)) {
            *self.0.owner.lock().expect("owner") = owner_with_label(1, 4, 0, "changed view");
        }
        self.0.request(request)
    }
}

#[test]
fn selected_graph_center_authentication_refuses_a_rebound_source_without_refreshing_revision() {
    let (owner, requests, symbol, ids) = selected_page_fixture();
    let mut client = Session::from_transport(
        "/private/test-owner.sock",
        RebindBeforeSelectedDocument(Transport {
            owner,
            requests: requests.clone(),
            graph_ids: Some((symbol, ids)),
        }),
    );
    let window = backend_library::QueryWindow::fixed(QueryLimit::new(1).expect("one row"));
    assert!(
        client
            .graph_page_declaration(&selected_at(symbol, "actual label"), window, None)
            .is_err()
    );
    let commands = requests.lock().expect("requests");
    assert_eq!(
        commands
            .iter()
            .filter(|command| matches!(command, Command::Revision))
            .count(),
        1
    );
    assert!(
        !commands
            .iter()
            .any(|command| matches!(command, Command::GraphPage { .. }))
    );
    assert!(client.continuations.is_empty());
}
fn page(reply: &ReplyDto) -> &backend_library::ViewSnapshot {
    match &reply.reply {
        CommandReply::Search(page) | CommandReply::Names(page) => page,
        _ => panic!("expected query page"),
    }
}
fn first_token(
    owner: &Arc<Mutex<Library>>,
    requests: &Arc<Mutex<Vec<Command>>>,
    names: bool,
    limit: u16,
) -> String {
    let mut issuing = session(owner, requests);
    let reply = if names {
        issuing.names_page("Thing", limit, None)
    } else {
        issuing.search_page("Thing", limit, None)
    }
    .expect("first page");
    issuing
        .encode_query_continuation(PageContinuation::from_cursor(
            page(&reply).next.expect("next"),
        ))
        .expect("export")
}
fn token_value(token: &str) -> serde_json::Value {
    serde_json::from_slice(&decode_portable_body(token).expect("bounded proof")).expect("envelope")
}
fn token_bytes(bytes: &[u8]) -> String {
    let mut token = String::from("pc2-");
    for byte in bytes {
        use fmt::Write as _;
        write!(token, "{byte:02x}").expect("hex");
    }
    token
}
fn token(value: &serde_json::Value) -> String {
    encode_compact_proof(&serde_json::to_vec(value).expect("wire")).expect("bounded compact tamper")
}

#[test]
fn portable_query_fresh_sessions_traverse_names_and_search_without_replay() {
    for names in [false, true] {
        let (owner, requests) = fixture();
        let mut issuing = session(&owner, &requests);
        let mut reply = if names {
            issuing.names_page("Thing", 3, None)
        } else {
            issuing.search_page("Thing", 3, None)
        }
        .expect("first page");
        let reference = {
            let library = owner.lock().expect("owner");
            if names {
                library.names(&NameQuery::new(
                    "Thing",
                    library.revision_root(),
                    QueryLimit::new(200).expect("limit"),
                ))
            } else {
                library.search(&Query::new(
                    "Thing",
                    library.revision_root(),
                    QueryLimit::new(200).expect("limit"),
                ))
            }
            .expect("reference")
            .root
            .rows()
            .iter()
            .map(|row| row.id)
            .collect::<Vec<_>>()
        };
        let mut identities = page(&reply)
            .root
            .rows()
            .iter()
            .map(|row| row.id)
            .collect::<Vec<_>>();
        let mut next = issuing
            .encode_query_continuation(PageContinuation::from_cursor(
                page(&reply).next.expect("next"),
            ))
            .expect("token");
        drop(issuing);
        let mut pages = 1;
        loop {
            let before = requests.lock().expect("requests").len();
            let mut fresh = session(&owner, &requests);
            assert!(fresh.continuations.is_empty());
            let continuation = fresh
                .decode_page_continuation(&next)
                .expect("fresh imported proof");
            reply = if names {
                fresh.names_page("Thing", 3, Some(continuation))
            } else {
                fresh.search_page("Thing", 3, Some(continuation))
            }
            .expect("resumed page");
            assert_eq!(
                requests.lock().expect("requests").len() - before,
                2,
                "fresh revision plus one exact next query"
            );
            identities.extend(page(&reply).root.rows().iter().map(|row| row.id));
            pages += 1;
            let Some(cursor) = page(&reply).next else {
                break;
            };
            next = fresh
                .encode_query_continuation(PageContinuation::from_cursor(cursor))
                .expect("successor");
        }
        assert_eq!(pages, 13);
        assert_eq!(identities, reference);
        assert_eq!(identities.len(), 37);
    }
}

#[test]
fn portable_query_scope_rebind_same_root_and_selected_root_change_are_refused() {
    for replacement in [owner(2, 37, 0), owner(1, 38, 0)] {
        let (owner, requests) = fixture();
        let token = first_token(&owner, &requests, false, 3);
        *owner.lock().expect("owner") = replacement;
        assert!(
            session(&owner, &requests)
                .decode_page_continuation(&token)
                .is_err()
        );
    }
}

#[test]
fn portable_query_intent_only_sequence_progress_keeps_old_proof_valid() {
    let (owner, requests) = fixture();
    let token = first_token(&owner, &requests, false, 3);
    *owner.lock().expect("owner") = self::owner(1, 37, 9);
    let mut fresh = session(&owner, &requests);
    let continuation = fresh
        .decode_page_continuation(&token)
        .expect("same exact view with newer stream");
    assert!(fresh.search_page("Thing", 3, Some(continuation)).is_ok());
}

#[test]
fn portable_query_external_family_text_and_credit_are_exact() {
    for (names, text, limit) in [
        (true, "Thing", 3),
        (false, "Other", 3),
        (false, "Thing", 1),
        (false, "Thing", 4),
    ] {
        let (owner, requests) = fixture();
        let token = first_token(&owner, &requests, false, 3);
        let mut fresh = session(&owner, &requests);
        let continuation = fresh.decode_page_continuation(&token).expect("proof");
        let before = requests.lock().expect("requests").len();
        assert!(
            if names {
                fresh.names_page(text, limit, Some(continuation))
            } else {
                fresh.search_page(text, limit, Some(continuation))
            }
            .is_err()
        );
        assert_eq!(
            requests.lock().expect("requests").len(),
            before,
            "wrong caller contract must not request a page"
        );
    }
}

#[test]
fn portable_query_malformed_oversized_and_unknown_envelopes_refuse_before_rpc() {
    let (owner, requests) = fixture();
    let good = first_token(&owner, &requests, false, 3);
    let mut unknown = token_value(&good);
    unknown["untrusted_extra"] = serde_json::json!(true);
    let mut version = token_value(&good);
    version["version"] = serde_json::json!(65535);
    for bad in [
        String::from("pc3-00"),
        String::from("pc2-xx"),
        String::from("pc2-0"),
        format!("pc2-{}", "00".repeat(MAX_PORTABLE_QUERY_TOKEN_BYTES)),
        token(&unknown),
        token(&version),
    ] {
        let before = requests.lock().expect("requests").len();
        assert!(
            session(&owner, &requests)
                .decode_page_continuation(&bad)
                .is_err()
        );
        assert_eq!(requests.lock().expect("requests").len(), before);
    }
}

#[test]
fn portable_query_tampered_contract_cursor_scope_and_preimage_fields_refuse() {
    let (owner, requests) = fixture();
    let good = first_token(&owner, &requests, false, 3);
    let original = token_value(&good);
    let mut mutants = vec![];
    for (field, value) in [
        ("text", serde_json::json!("Other")),
        ("limit", serde_json::json!(1)),
        ("limit", serde_json::json!(4)),
        ("read_manifest", serde_json::json!({"reads":[]})),
    ] {
        let mut changed = original.clone();
        changed["command"]["data"][field] = value;
        mutants.push(changed);
    }
    let mut family = original.clone();
    family["command"]["kind"] = serde_json::json!("name");
    mutants.push(family);
    for (field, value) in [
        ("query_offset", 0),
        ("query_offset", 6),
        ("sequence", 999),
        ("schema", 999),
    ] {
        let mut changed = original.clone();
        changed["command"]["data"]["cursor"][field] = serde_json::json!(value);
        mutants.push(changed);
    }
    let mut correlation = original.clone();
    correlation["request_id"] = serde_json::json!(2);
    mutants.push(correlation);
    let mut context = original.clone();
    let coverage = context["certificate"]["claims"]
        .as_array_mut()
        .expect("claims")
        .iter_mut()
        .find(|claim| claim["kind"] == "coverage")
        .expect("issuing scope");
    coverage["data"]["context"] = serde_json::json!(encode_id(&[2; 32]));
    mutants.push(context);
    for field in ["scope", "observed", "producer"] {
        let mut changed = original.clone();
        let coverage = changed["certificate"]["claims"]
            .as_array_mut()
            .expect("claims")
            .iter_mut()
            .find(|claim| claim["kind"] == "coverage")
            .expect("scope");
        coverage["data"][field] = serde_json::json!(encode_id(
            object_version(b"foreign-projection-scope").as_bytes()
        ));
        mutants.push(changed);
    }
    for (field, schema, id) in [
        (
            "branch",
            "branch",
            encode_id(backend_library::branch_key("foreign").as_bytes()),
        ),
        (
            "log",
            "log",
            encode_id(backend_library::log_key("foreign").as_bytes()),
        ),
    ] {
        let mut changed = original.clone();
        changed["command"]["data"]["cursor"][field] = serde_json::json!(id);
        let claim = changed["certificate"]["claims"]
            .as_array_mut()
            .expect("claims")
            .iter_mut()
            .find(|claim| claim["kind"] == "key" && claim["data"]["schema"] == schema)
            .expect("canonical owner stream claim");
        claim["data"]["id"] = serde_json::json!(id);
        claim["data"]["value"] = serde_json::json!("foreign");
        mutants.push(changed);
    }
    let mut preimage = original.clone();
    let version = preimage["certificate"]["claims"]
        .as_array_mut()
        .expect("claims")
        .iter_mut()
        .find(|claim| claim["kind"] == "version")
        .expect("predecessor version");
    version["data"]["value"][0] = serde_json::json!(255);
    mutants.push(preimage);
    for (index, changed) in mutants.iter().enumerate() {
        let mut fresh = session(&owner, &requests);
        let outcome = fresh
            .decode_page_continuation(&token(changed))
            .and_then(|cursor| fresh.search_page("Thing", 3, Some(cursor)));
        assert!(outcome.is_err(), "tampered token case {index} admitted");
    }
}

#[test]
fn portable_query_duplicate_json_fields_are_not_normalized_into_a_proof() {
    let (owner, requests) = fixture();
    let good = first_token(&owner, &requests, false, 3);
    let body = decode_portable_body(&good).expect("bounded proof");
    let original = String::from_utf8(body).expect("JSON");
    let duplicate = format!(
        "{{\"request_id\":1,{}",
        original.strip_prefix('{').expect("object")
    );
    let before = requests.lock().expect("requests").len();
    assert!(
        session(&owner, &requests)
            .decode_page_continuation(&token_bytes(duplicate.as_bytes()))
            .is_err()
    );
    assert_eq!(requests.lock().expect("requests").len(), before);
}

#[test]
fn portable_query_full_credit_can_export_a_full_root_predecessor() {
    let owner = Arc::new(Mutex::new(owner(1, 401, 0)));
    let requests = Arc::new(Mutex::new(vec![]));
    let mut session = session(&owner, &requests);
    let reply = session
        .names_page("Thing", 200, None)
        .expect("full credit query");
    let next = page(&reply).next.expect("successor exists");
    let exported = session.encode_query_continuation(PageContinuation::from_cursor(next));
    assert!(exported.is_ok(), "full-root export: {exported:?}");
}

#[test]
fn portable_query_compressible_long_proof_preserves_every_canonical_byte() {
    let text = "x".repeat(backend_library::MAX_COMMAND_TEXT);
    let owner = Arc::new(Mutex::new(owner_with_label(1, 4, 0, &text)));
    let requests = Arc::new(Mutex::new(vec![]));
    let mut issuing = session(&owner, &requests);
    let reply = issuing
        .names_page(&text, 3, None)
        .expect("admitted bounded long query");
    let next = page(&reply).next.expect("successor exists");
    let exported = issuing.encode_query_continuation(PageContinuation::from_cursor(next));
    let token = exported.expect("bounded full-root proof");
    assert!(token.starts_with("pc4-"));
    let body = decode_portable_body(&token).expect("bounded expansion");
    let full: CommandDto = serde_json::from_slice(&body).expect("bounded full-root command");
    assert!(full_root_catalog_continuation_shape(&full));
    assert!(
        full.certificate()
            .expect("certificate")
            .claims
            .iter()
            .any(|claim| matches!(
                claim,
                WireClaim::Root {
                    schema: WireSchema::ViewRelation,
                    ..
                }
            ))
    );
    assert!(
        backend_library::decode_command_body(&body).is_ok(),
        "full page-root preimage must pass the strict standalone decoder"
    );

    let mut fresh = session(&owner, &requests);
    let continuation = fresh
        .decode_page_continuation(&token)
        .expect("cold owner reopens full-root token");
    let resumed = fresh
        .names_page(&text, 3, Some(continuation))
        .expect("exact cold continuation");
    assert_eq!(page(&resumed).next, None);
}

#[test]
fn portable_search_full_root_preserves_exact_witness_and_page_union() {
    let owner = Arc::new(Mutex::new(owner(1, 9, 0)));
    let requests = Arc::new(Mutex::new(vec![]));
    let mut issuing = session(&owner, &requests);
    let first = issuing.search_page("Thing", 3, None).expect("first page");
    let cursor = page(&first).next.expect("second page exists");
    let original = issuing.continuations[&cursor]
        .next_request
        .as_ref()
        .expect("issued exact next command")
        .clone();
    let certificate = original.certificate().expect("full certificate");
    let page_root_id = encode_id(cursor.root().as_bytes());
    let _page_root = certificate
        .claims
        .iter()
        .find(|claim| {
            matches!(claim, WireClaim::Root { schema: WireSchema::ViewRelation, id, .. } if id == &page_root_id)
        })
        .expect("canonical page root preimage remains present");
    let owner_cursor = owner.lock().expect("owner").cursor();
    assert_ne!(cursor.root(), owner_cursor.root());
    let mut rootless_claims = certificate
        .claims
        .iter()
        .filter(|claim| match claim {
            WireClaim::Root {
                schema: WireSchema::ViewRelation,
                id,
                ..
            } => id.as_str() != page_root_id.as_str(),
            _ => true,
        })
        .cloned()
        .collect::<Vec<_>>();
    rootless_claims.push(WireClaim::RootCommitment {
        schema: WireSchema::ViewRelation,
        id: page_root_id.clone(),
    });
    let rootless_certificate = WireCertificate::from_claims(rootless_claims.into_boxed_slice());
    let rootless = CommandDto::new(original.request_id, original.command.clone())
        .with_certificate(rootless_certificate);
    let rootless_body = serde_json::to_vec(&rootless).expect("rootless envelope");
    assert!(backend_library::decode_command_body_for_owner(&rootless_body, owner_cursor).is_err());

    let portable_token = issuing
        .encode_query_continuation(PageContinuation::from_cursor(cursor))
        .expect("portable full-root token");
    assert!(portable_token.starts_with("pc4-"));
    let body = decode_portable_body(&portable_token).expect("bounded expansion");
    let reopened = backend_library::decode_command_body(&body)
        .expect("full-root command passes standalone strict decoding");
    assert_eq!(reopened.command, original.command);
    assert_eq!(reopened.certificate(), original.certificate());
    assert!(backend_library::decode_command_body_for_owner(&body, owner_cursor).is_ok());

    let mut forged_root = token_value(&portable_token);
    let forged_page_root = encode_id(view_state_root(&[]).as_bytes());
    let root_claim = forged_root["certificate"]["claims"]
        .as_array_mut()
        .expect("claims")
        .iter_mut()
        .find(|claim| {
            claim["kind"] == "root"
                && claim["data"]["schema"] == "view_relation"
                && claim["data"]["id"] == page_root_id
        })
        .expect("page root claim");
    root_claim["data"]["id"] = serde_json::json!(forged_page_root);
    forged_root["command"]["data"]["cursor"]["root"] = serde_json::json!(forged_page_root);
    let search_requests_before = requests
        .lock()
        .expect("requests")
        .iter()
        .filter(|command| matches!(command, Command::Search(_)))
        .count();
    let mut reject_forged = session(&owner, &requests);
    assert!(
        reject_forged
            .decode_page_continuation(&token(&forged_root))
            .is_err()
    );
    assert_eq!(
        requests
            .lock()
            .expect("requests")
            .iter()
            .filter(|command| matches!(command, Command::Search(_)))
            .count(),
        search_requests_before,
        "a changed root id cannot pass strict canonical-preimage admission"
    );

    let mut forged_query = token_value(&portable_token);
    forged_query["command"]["data"]["text"] = serde_json::json!("Different query");
    let mut reject_query = session(&owner, &requests);
    assert!(
        reject_query
            .decode_page_continuation(&token(&forged_query))
            .is_err()
    );
    assert_eq!(
        requests
            .lock()
            .expect("requests")
            .iter()
            .filter(|command| matches!(command, Command::Search(_)))
            .count(),
        search_requests_before,
        "a changed query cannot be accepted with the original certificate"
    );

    let expected = issuing
        .search_page("Thing", 3, Some(PageContinuation::from_cursor(cursor)))
        .expect("warm successor");
    drop(issuing);
    let mut fresh = session(&owner, &requests);
    let reopened = fresh
        .decode_page_continuation(&portable_token)
        .expect("cold durable owner reopens full-root token");
    let actual = fresh
        .search_page("Thing", 3, Some(reopened))
        .expect("cold successor");
    assert_eq!(page(&actual), page(&expected), "cold page equals warm page");
    let third = fresh
        .search_page(
            "Thing",
            3,
            page(&actual).next.map(PageContinuation::from_cursor),
        )
        .expect("third page");
    let mut union = page(&first).root.rows().to_vec();
    union.extend_from_slice(page(&actual).root.rows());
    union.extend_from_slice(page(&third).root.rows());
    let all = session(&owner, &requests)
        .search_page("Thing", 200, None)
        .expect("single full page");
    assert_eq!(
        union,
        page(&all).root.rows(),
        "portable page union equals full search"
    );
}

struct RebindingTransport(Transport);
impl CommandTransport for RebindingTransport {
    fn request(&mut self, request: CommandDto) -> Result<ReplyDto, ClientError> {
        if matches!(&request.command, Command::Search(_) | Command::Name(_)) {
            let mut current = self.0.owner.lock().expect("owner");
            *current = owner(2, current.view().rows().len(), current.cursor().sequence());
        }
        self.0.request(request)
    }
}

#[test]
fn portable_query_terminal_reply_cannot_bypass_a_scope_rebind_between_rpcs() {
    let owner = Arc::new(Mutex::new(owner(1, 4, 0)));
    let requests = Arc::new(Mutex::new(vec![]));
    let mut issuing = session(&owner, &requests);
    let first = issuing.search_page("Thing", 3, None).expect("first");
    let token = issuing
        .encode_query_continuation(PageContinuation::from_cursor(
            page(&first).next.expect("one-row terminal successor"),
        ))
        .expect("bounded proof");
    let mut fresh = Session::from_transport(
        "/private/test-owner.sock",
        RebindingTransport(Transport {
            owner,
            requests,
            graph_ids: None,
        }),
    );
    let cursor = fresh
        .decode_page_continuation(&token)
        .expect("revision admits old scope");
    assert!(
        fresh.search_page("Thing", 3, Some(cursor)).is_err(),
        "same root/new scope terminal page must refuse"
    );
}

#[test]
fn portable_query_coherently_rederived_authorized_prefix_is_content_proof_not_issue_receipt() {
    let (owner, requests) = fixture();
    let mut issuing = session(&owner, &requests);
    let first = issuing
        .names_page("Thing", 3, None)
        .expect("issued original query");
    let original = issuing
        .encode_query_continuation(PageContinuation::from_cursor(
            page(&first).next.expect("successor"),
        ))
        .expect("original token");
    let mut body: serde_json::Value =
        serde_json::from_slice(&decode_portable_body(&original).expect("bounded proof"))
            .expect("existing DTO");
    let projected = &page(&first).root;
    let selected_root = owner.lock().expect("owner").revision_root();
    let new_query = NameQuery::new("thing", selected_root, QueryLimit::new(3).expect("credit"));
    let recipe = QueryPageRecipe::names(selected_root, &new_query);
    // No second query page is requested from the owner. These are public,
    // unkeyed canonical content preimages for the same authorized prefix.
    let rederived = ViewRoot::new_checked(
        recipe.identity(),
        projected.basis(),
        projected.frontier(),
        projected.rows().to_vec(),
        projected.coverage().to_vec(),
        projected.capability().expect("owned fixture scope"),
    )
    .expect("canonical rederived projection");
    assert_eq!(
        rederived.root(),
        projected.root(),
        "case-folded name query selects identical rows"
    );
    body["command"]["data"]["text"] = serde_json::json!("thing");
    body["command"]["data"]["cursor"]["recipe"] =
        serde_json::json!(encode_id(recipe.identity().as_bytes()));
    body["command"]["data"]["cursor"]["version"] =
        serde_json::json!(encode_id(rederived.version().as_bytes()));
    for claim in body["certificate"]["claims"]
        .as_array_mut()
        .expect("claims")
    {
        if claim["kind"] == "key_bytes" && claim["data"]["schema"] == "view_recipe" {
            claim["data"]["id"] = serde_json::json!(encode_id(recipe.identity().as_bytes()));
            claim["data"]["value"] = serde_json::json!(recipe.canonical_preimage());
        }
        if claim["kind"] == "version" && claim["data"]["schema"] == "view_version" {
            claim["data"]["id"] = serde_json::json!(encode_id(rederived.version().as_bytes()));
            claim["data"]["value"] = serde_json::json!(view_version_preimage(
                rederived.recipe(),
                rederived.basis(),
                rederived.frontier(),
                rederived.root(),
                rederived.coverage()
            ));
        }
    }
    let before = requests.lock().expect("requests").len();
    let mut fresh = session(&owner, &requests);
    let cursor = fresh
        .decode_page_continuation(&token(&body))
        .expect("fresh authority admits canonical content proof");
    let reply = fresh
        .names_page("thing", 3, Some(cursor))
        .expect("owner reconstructs exact new-query predecessor");
    assert_eq!(requests.lock().expect("requests").len() - before, 2);
    let expected = owner
        .lock()
        .expect("owner")
        .names(&NameQuery::new(
            "thing",
            selected_root,
            QueryLimit::new(200).expect("reference"),
        ))
        .expect("independent current view")
        .root
        .rows()
        .iter()
        .skip(3)
        .take(3)
        .map(|row| row.id)
        .collect::<Vec<_>>();
    assert_eq!(
        page(&reply)
            .root
            .rows()
            .iter()
            .map(|row| row.id)
            .collect::<Vec<_>>(),
        expected
    );
}

#[test]
fn portable_compact_proof_rejects_expansion_truncation_and_trailing_bytes() {
    let oversized = vec![b'x'; backend_library::MAX_COMMAND_BODY + 1];
    assert!(encode_compact_proof(&oversized).is_err());
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    Write::write_all(&mut encoder, &oversized).expect("compressed adversary");
    let compressed = encoder.finish().expect("compressed adversary");
    let bomb = format!("pc3-{}", URL_SAFE_NO_PAD.encode(&compressed));
    assert!(bomb.len() < MAX_PORTABLE_QUERY_TOKEN_BYTES);
    assert!(matches!(
        decode_portable_body(&bomb),
        Err(ClientError::Transport(ReplicationError::MessageTooLarge))
    ));
    let good = encode_compact_proof(b"bounded canonical body").expect("compact");
    let mut compressed = URL_SAFE_NO_PAD
        .decode(good.strip_prefix("pc3-").expect("version"))
        .expect("base64");
    compressed.push(0);
    assert!(decode_portable_body(&format!("pc3-{}", URL_SAFE_NO_PAD.encode(&compressed))).is_err());
    compressed.truncate(compressed.len() - 3);
    assert!(decode_portable_body(&format!("pc3-{}", URL_SAFE_NO_PAD.encode(&compressed))).is_err());
}

#[test]
fn portable_zlib_stream_requires_complete_header_body_checksum_and_canonical_base64() {
    let good = encode_compact_proof(b"complete zlib proof body").expect("compact");
    let compressed = URL_SAFE_NO_PAD
        .decode(good.strip_prefix("pc3-").expect("version"))
        .expect("base64");
    for end in [1, 2, compressed.len() / 2, compressed.len() - 4] {
        assert!(
            decode_portable_body(&format!(
                "pc3-{}",
                URL_SAFE_NO_PAD.encode(&compressed[..end])
            ))
            .is_err(),
            "truncated stream at byte {end} must fail"
        );
    }
    assert!(decode_portable_body(&format!("{good}=")).is_err());
}

#[test]
fn binary_portable_proof_bounds_canonical_encoding_and_format_are_closed_before_rpc() {
    let (owner, requests) = fixture();
    let good = first_token(&owner, &requests, false, 3);
    assert!(good.starts_with("pc4-"));
    let request = backend_library::decode_command_body(&decode_portable_body(&good).expect("JSON"))
        .expect("strict command");
    let binary = backend_library::encode_binary_certificate_command(&request).expect("binary");
    let mut compressed = URL_SAFE_NO_PAD.decode(&good[4..]).expect("base64");
    let mut trailing = compressed.clone();
    trailing.push(0);
    compressed.pop();
    let mut bad_version = binary.clone();
    bad_version[5] = 2;
    let mut bad_length = binary.clone();
    bad_length[6..10].copy_from_slice(&u32::MAX.to_be_bytes());
    let mut corrupted = binary.clone();
    // The complete predecessor root is the final claim in this issued proof.
    assert_eq!(
        token_value(&good)["certificate"]["claims"]
            .as_array()
            .expect("claims")
            .last()
            .expect("root")["kind"],
        "root"
    );
    let end = corrupted.len() - 1;
    corrupted[end] ^= 1;
    let mut extended = binary.clone();
    extended.push(0);
    let json = decode_portable_body(&good).expect("original JSON");
    let mutants = [
        format!("pc4-{}", URL_SAFE_NO_PAD.encode(trailing)),
        format!("pc4-{}", URL_SAFE_NO_PAD.encode(compressed)),
        format!("{good}="),
        "pc4-Zh".to_owned(),
        good.replacen("pc4-", "pc3-", 1),
        encode_portable_proof(&bad_version, "pc4-").expect("bad version compression"),
        encode_portable_proof(&bad_length, "pc4-").expect("bad length compression"),
        encode_portable_proof(&corrupted, "pc4-").expect("bad proof compression"),
        encode_portable_proof(&extended, "pc4-").expect("trailing frame compression"),
        encode_portable_proof(&json, "pc4-").expect("JSON is not binary"),
    ];
    for bad in mutants {
        let before = requests.lock().expect("requests").len();
        assert!(
            session(&owner, &requests)
                .decode_page_continuation(&bad)
                .is_err()
        );
        assert_eq!(requests.lock().expect("requests").len(), before);
    }
    let oversized = vec![0; backend_library::MAX_COMMAND_BODY + 1];
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    Write::write_all(&mut encoder, &oversized).expect("compressed adversary");
    let bomb = format!(
        "pc4-{}",
        URL_SAFE_NO_PAD.encode(encoder.finish().expect("finish"))
    );
    assert!(bomb.len() < MAX_PORTABLE_QUERY_TOKEN_BYTES);
    let before = requests.lock().expect("requests").len();
    assert!(matches!(
        session(&owner, &requests).decode_page_continuation(&bomb),
        Err(ClientError::Transport(ReplicationError::MessageTooLarge))
    ));
    assert_eq!(requests.lock().expect("requests").len(), before);
}

#[test]
fn binary_and_legacy_query_proofs_reopen_exact_page_after_owner_and_session_restart() {
    for names in [false, true] {
        let owner = Arc::new(Mutex::new(owner(1, 4, 7)));
        let requests = Arc::new(Mutex::new(vec![]));
        let mut issuing = session(&owner, &requests);
        let first = if names {
            issuing.names_page("Thing", 1, None)
        } else {
            issuing.search_page("Thing", 1, None)
        }
        .expect("first");
        let next = PageContinuation::from_cursor(page(&first).next.expect("next"));
        let binary = issuing.encode_query_continuation(next).expect("pc4 export");
        let body = decode_portable_body(&binary).expect("exact restored proof");
        let expected = if names {
            issuing.names_page("Thing", 1, Some(next))
        } else {
            issuing.search_page("Thing", 1, Some(next))
        }
        .expect("warm second");
        drop(issuing);
        for token in [
            binary,
            token_bytes(&body),
            encode_compact_proof(&body).expect("pc3"),
        ] {
            *owner.lock().expect("owner") = self::owner(1, 4, 7);
            let mut fresh = session(&owner, &requests);
            assert!(fresh.continuations.is_empty());
            let decoded = fresh.decode_page_continuation(&token).expect("cold proof");
            assert_eq!(decoded, next);
            let actual = if names {
                fresh.names_page("Thing", 1, Some(decoded))
            } else {
                fresh.search_page("Thing", 1, Some(decoded))
            }
            .expect("cold exact page");
            assert_eq!(page(&actual), page(&expected));
        }
    }
}

#[test]
fn portable_names_public_rank_reproduces_full_rows_across_fresh_pages() {
    let owner = Arc::new(Mutex::new(owner_with_labels(
        1,
        37,
        0,
        &[
            "Thing",
            "Thing repeated",
            "ThingZZ",
            "anotherThing",
            "Thingα路径",
        ],
    )));
    let requests = Arc::new(Mutex::new(vec![]));
    let reference = owner.lock().expect("owner");
    let mut expected = reference
        .names(&NameQuery::new(
            "Thing",
            reference.revision_root(),
            QueryLimit::new(200).expect("credit"),
        ))
        .expect("complete reference")
        .root
        .rows()
        .to_vec();
    drop(reference);
    let storage_order = expected.iter().map(|row| row.id).collect::<Vec<_>>();
    expected.sort_by_key(|row| std::cmp::Reverse(row.score));
    assert_ne!(
        storage_order,
        expected.iter().map(|row| row.id).collect::<Vec<_>>(),
        "fixture must exercise names order different from canonical storage"
    );
    let mut seen = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut fresh = session(&owner, &requests);
        let continuation = token
            .as_deref()
            .map(|token| fresh.decode_page_continuation(token).expect("fresh scope"));
        let reply = fresh.names_page("Thing", 3, continuation).expect("page");
        let mut rows = page(&reply).root.rows().to_vec();
        rows.sort_by_key(|row| std::cmp::Reverse(row.score));
        seen.extend(rows);
        token = page(&reply).next.map(|cursor| {
            fresh
                .encode_query_continuation(PageContinuation::from_cursor(cursor))
                .expect("bounded token")
        });
        if token.is_none() {
            break;
        }
    }
    assert_eq!(
        seen, expected,
        "all public row fields, ranks, order and multiplicities must match"
    );
}

#[test]
fn portable_compact_proof_retains_real_cachetools_dto_and_exact_expansion_bound() {
    // Exact retained public Cachetools proof from f44c1bdd3c, Git blob
    // eba1c588c224ae1bd8affb7d9ce0bf2314811c90 (SHA-256
    // a4467416cd0dc60a6315e0d952495e9d82519f2522de99c841f1e5b63422d15f).
    let mut value: serde_json::Value =
        serde_json::from_slice(include_bytes!("fixtures/real-cachetools-first-token.json"))
            .expect("retained actual DTO");
    let body = serde_json::to_vec(&value).expect("canonical JSON");
    let token = encode_compact_proof(&body).expect("ordinary real proof fits");
    assert!(
        token.len() < 4096,
        "ordinary canonical proof should fit compact presentation"
    );
    assert_eq!(decode_portable_body(&token).expect("compact bytes"), body);
    let legacy = backend_library::decode_command_body(&body)
        .expect_err("the original historical envelope retains its old wire version");
    assert!(legacy.contains("unsupported command DTO version 20"));
    // Re-envelope the retained canonical claims at this build's version.
    // The fixture bytes remain the authentic version-20 capture above.
    value["version"] = serde_json::json!(backend_library::DTO_VERSION);
    backend_library::decode_command_body(&serde_json::to_vec(&value).expect("current envelope"))
        .expect("all retained real canonical claims rehash strictly at the current version");
    assert_eq!(
        decode_portable_body(&token_bytes(&body)).expect("legacy pc2 bytes"),
        body
    );
    let exact = vec![b'x'; backend_library::MAX_COMMAND_BODY];
    let token = encode_compact_proof(&exact).expect("exact decoded ceiling");
    assert_eq!(
        decode_portable_body(&token).expect("exact decoded ceiling"),
        exact
    );
    let (owner, requests) = fixture();
    let good = first_token(&owner, &requests, false, 3);
    let mut body = decode_portable_body(&good).expect("canonical body");
    body.extend_from_slice(b" {}");
    let extra = encode_compact_proof(&body).expect("bounded extra JSON");
    let before = requests.lock().expect("requests").len();
    assert!(
        session(&owner, &requests)
            .decode_page_continuation(&extra)
            .is_err()
    );
    assert_eq!(requests.lock().expect("requests").len(), before);
}

#[test]
fn graph_query_portable_continuation_reopens_in_fresh_session() {
    let owner = Arc::new(Mutex::new(owner(1, 3, 7)));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let input =
        AdmittedGraphQueryInput::new("{ Declaration { coordinate @output } }", BTreeMap::new())
            .expect("query");
    let mut first = session(&owner, &requests);
    let page = first
        .graph_query_admitted(input.clone(), 1, None, false)
        .expect("first admitted page");
    let PageTerminal::More(next) = page.terminal else {
        panic!("next page");
    };
    let token = first
        .encode_query_continuation(next)
        .expect("portable graph proof");
    assert!(token.starts_with("pc4-"));
    assert!(token.len() <= MAX_PORTABLE_QUERY_TOKEN_BYTES);
    drop(first);
    // An intent-only owner advance retains the exact view and authority scope.
    *owner.lock().expect("owner") = self::owner(1, 3, 8);
    let mut fresh = session(&owner, &requests);
    let decoded = fresh
        .decode_page_continuation(&token)
        .expect("cold public token");
    assert_eq!(decoded, next);
    let second = fresh
        .graph_query_admitted(input.clone(), 1, Some(decoded), false)
        .expect("cold resumed admitted wire");
    assert_eq!(
        second.rows[0].fields()[0].1,
        GraphValue::String("Thing1".to_owned())
    );
    for (changed_input, changed_limit) in [
        (input.clone(), 2),
        (
            AdmittedGraphQueryInput::new("{ Declaration { name @output } }", BTreeMap::new())
                .expect("changed"),
            1,
        ),
        (
            AdmittedGraphQueryInput::new(
                input.query(),
                BTreeMap::from([("value".to_owned(), GraphValue::Unsigned(2))]),
            )
            .expect("variables"),
            1,
        ),
    ] {
        let mut fresh = session(&owner, &requests);
        let decoded = fresh.decode_page_continuation(&token).expect("proof");
        assert!(
            fresh
                .graph_query_admitted(changed_input, changed_limit, Some(decoded), false)
                .is_err()
        );
    }
    let mut cancelling = session(&owner, &requests);
    let decoded = cancelling
        .decode_page_continuation(&token)
        .expect("cold cancellation proof");
    let cancelled = cancelling
        .graph_query_admitted(input, 1, Some(decoded), true)
        .expect("cancel exact query");
    assert!(cancelled.rows.is_empty());
    assert_eq!(cancelled.terminal, PageTerminal::Cancelled);
    let original = token_value(&token);
    for field in ["scope", "observed", "producer", "context"] {
        let mut forged = original.clone();
        let coverage = forged["certificate"]["claims"]
            .as_array_mut()
            .expect("claims")
            .iter_mut()
            .find(|claim| claim["kind"] == "coverage")
            .expect("scope");
        coverage["data"][field] = serde_json::json!("00".repeat(32));
        assert!(
            session(&owner, &requests)
                .decode_page_continuation(&token_bytes(
                    &serde_json::to_vec(&forged).expect("forged")
                ))
                .is_err()
        );
    }
    *owner.lock().expect("owner") = self::owner(1, 4, 9);
    assert!(
        session(&owner, &requests)
            .decode_page_continuation(&token)
            .is_err(),
        "changed current view"
    );
}

fn graph_fixture(selected_neighbors: bool, context: u8, sequence: u64) -> Library {
    let prototype = owner(context, 1, sequence);
    let view = prototype.view();
    let basis = view.basis();
    let package = package_key("graph");
    let source = symbol_key("graph::source");
    let center = if selected_neighbors {
        Row::in_package(
            RowId::Symbol(source),
            basis,
            package,
            "graph::semantic::compiler-origin::source",
        )
        .try_with_identity_preimage("graph::source")
        .expect("exact native identity preimage")
        .with_source(
            backend_library::SourceLocation::new("src/source.go", 7).expect("captured source"),
        )
    } else {
        Row::in_package(RowId::Symbol(source), basis, package, "graph::source")
    };
    let mut rows = vec![Row::new(RowId::Package(package), basis, "graph"), center];
    rows.extend((0..6).map(|i| {
        let row = Row::in_package(
            RowId::Symbol(symbol_key(&format!("graph::neighbor{i}"))),
            basis,
            package,
            format!("graph::neighbor{i}"),
        );
        if selected_neighbors {
            row
        } else {
            row.with_parent(source)
        }
    }));
    let root = ViewRoot::new_checked(
        view.recipe(),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
        rows,
        vec![Coverage::Complete],
        view.capability().expect("owned scope"),
    )
    .expect("graph fixture root");
    Library::from_view(root.clone(), Cursor::for_view_root_at(&root, sequence))
        .expect("graph owner")
}

#[test]
fn graph_portable_fixture_owns_its_captured_alias_and_refuses_an_invented_locator() {
    let library = graph_fixture(true, 1, 7);
    let native = symbol_key("graph::source");
    let center = library
        .view()
        .row(RowId::Symbol(native))
        .expect("native center");
    let package = library
        .view()
        .row(RowId::Package(package_key("graph")))
        .expect("owned package");
    let proof = backend_library::CapturedSourceCoordinate::from_row_and_package(&center, &package)
        .expect("actual captured ownership");
    let alias = "graph::src/source.go:7::source";
    assert!(proof.matches(alias));
    assert!(!proof.matches("copied::semantic::source"));
    assert_ne!(proof.address_key(), Some(native));
    let mut ids = library
        .view()
        .row_refs()
        .filter_map(|row| matches!(row.id, RowId::Symbol(_)).then_some(row.id))
        .collect::<Vec<_>>();
    ids.sort();
    let page = PageRequest::new(library.revision_root(), QueryLimit::new(2).expect("credit"));
    library
        .graph_page_from_ids(
            native,
            SymbolAddress::canonical(symbol_key(alias)),
            page,
            &ids,
        )
        .expect("real alias admits the exact native center");
    assert!(
        library
            .graph_page_from_ids(
                native,
                SymbolAddress::canonical(symbol_key("copied::semantic::source")),
                page,
                &ids,
            )
            .is_err()
    );
}

fn graph_session(
    owner: &Arc<Mutex<Library>>,
    requests: &Arc<Mutex<Vec<Command>>>,
    selected_neighbors: bool,
) -> Session {
    let graph_ids = selected_neighbors.then(|| {
        let library = owner.lock().expect("owner");
        let mut ids = library
            .view()
            .row_refs()
            .filter_map(|row| matches!(row.id, RowId::Symbol(_)).then_some(row.id))
            .collect::<Vec<_>>();
        ids.sort();
        (symbol_key("graph::source"), ids)
    });
    Session::from_transport(
        "/private/fresh-graph-owner.sock",
        Transport {
            owner: owner.clone(),
            requests: requests.clone(),
            graph_ids,
        },
    )
}

fn graph_page(reply: &ReplyDto) -> &backend_library::ProjectionPage {
    let CommandReply::ProjectionPage(page) = &reply.reply else {
        panic!("plain graph page")
    };
    page
}

#[test]
fn plain_graph_portable_continuation_reopens_exact_page_in_fresh_owner() {
    for selected_neighbors in [false, true] {
        // In the selected case the canonical copied coordinate differs from the
        // producer key, and neighbors have no structural parent relationship.
        let coordinate = if selected_neighbors {
            "graph::src/source.go:7::source"
        } else {
            "graph::source"
        };
        let owner = Arc::new(Mutex::new(graph_fixture(selected_neighbors, 1, 7)));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let mut issuing = graph_session(&owner, &requests, selected_neighbors);
        let first = issuing
            .graph_page(coordinate, 2, None)
            .expect("first graph page");
        let PageTerminal::More(next) = graph_page(&first).terminal else {
            panic!("next")
        };
        assert!(issuing.has_portable_query_continuation(next));
        let token = issuing
            .encode_query_continuation(next)
            .expect("portable graph");
        assert!(token.starts_with("pc4-"));
        backend_library::decode_command_body(&decode_portable_body(&token).expect("bytes"))
            .expect("strict canonical graph proof");
        // Existing pc1 remains valid in the compatible issuing active session.
        let legacy = issuing.encode_page_continuation(next);
        assert_eq!(
            issuing
                .decode_page_continuation(&legacy)
                .expect("active legacy"),
            next
        );
        let expected = issuing
            .graph_page(coordinate, 2, Some(next))
            .expect("warm second");
        drop(issuing);
        // Reconstruct the producer itself with identical immutable view/scope.
        *owner.lock().expect("owner") = graph_fixture(selected_neighbors, 1, 7);
        let mut fresh = graph_session(&owner, &requests, selected_neighbors);
        assert!(fresh.continuations.is_empty());
        assert!(fresh.decode_page_continuation(&legacy).is_err());
        let before = requests.lock().expect("requests").len();
        let imported = fresh
            .decode_page_continuation(&token)
            .expect("cold graph proof");
        let actual = fresh
            .graph_page(coordinate, 2, Some(imported))
            .expect("cold exact second");
        assert_eq!(graph_page(&actual), graph_page(&expected));
        assert_eq!(
            requests.lock().expect("requests").len() - before,
            2,
            "one fresh authority read and one exact page, no full graph materialization"
        );
        assert!(graph_page(&actual).snapshot.root.rows().len() <= 2);
        let mut ids = graph_page(&first)
            .snapshot
            .root
            .rows()
            .iter()
            .map(|row| row.id)
            .collect::<Vec<_>>();
        ids.extend(
            graph_page(&actual)
                .snapshot
                .root
                .rows()
                .iter()
                .map(|row| row.id),
        );
        let mut reply = actual;
        while let PageTerminal::More(next) = graph_page(&reply).terminal {
            let token = fresh.encode_query_continuation(next).expect("successor");
            fresh = graph_session(&owner, &requests, selected_neighbors);
            let continuation = fresh
                .decode_page_continuation(&token)
                .expect("fresh successor");
            reply = fresh
                .graph_page(coordinate, 2, Some(continuation))
                .expect("next bounded page");
            ids.extend(
                graph_page(&reply)
                    .snapshot
                    .root
                    .rows()
                    .iter()
                    .map(|row| row.id),
            );
        }
        let unique = ids
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(ids.len(), 7);
        assert_eq!(
            unique.len(),
            7,
            "stable neighborhood ordering without duplication"
        );
    }
}

#[test]
fn plain_graph_portable_continuation_refuses_foreign_contract_view_and_forgery() {
    let owner = Arc::new(Mutex::new(graph_fixture(false, 1, 0)));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let mut issuing = graph_session(&owner, &requests, false);
    let first = issuing.graph_page("graph::source", 2, None).expect("first");
    let PageTerminal::More(next) = graph_page(&first).terminal else {
        panic!("next")
    };
    let good = issuing.encode_query_continuation(next).expect("token");
    for (coordinate, limit) in [
        ("foreign::source", 2),
        ("graph::source", 1),
        ("graph::source", 3),
    ] {
        let mut fresh = graph_session(&owner, &requests, false);
        let next = fresh.decode_page_continuation(&good).expect("proof");
        let before = requests.lock().expect("requests").len();
        assert!(fresh.graph_page(coordinate, limit, Some(next)).is_err());
        assert_eq!(requests.lock().expect("requests").len(), before);
    }
    let mut fresh = graph_session(&owner, &requests, false);
    let next = fresh.decode_page_continuation(&good).expect("proof");
    let before = requests.lock().expect("requests").len();
    assert!(fresh.search_page("graph::source", 2, Some(next)).is_err());
    assert_eq!(requests.lock().expect("requests").len(), before);
    for replacement in [graph_fixture(false, 2, 0), graph_fixture(true, 1, 0)] {
        *owner.lock().expect("owner") = replacement;
        assert!(
            graph_session(&owner, &requests, false)
                .decode_page_continuation(&good)
                .is_err()
        );
    }
    *owner.lock().expect("owner") = graph_fixture(false, 1, 9);
    let mut progressed = graph_session(&owner, &requests, false);
    let next = progressed
        .decode_page_continuation(&good)
        .expect("intent-only sequence advance");
    assert!(
        progressed
            .graph_page("graph::source", 2, Some(next))
            .is_ok()
    );
    *owner.lock().expect("owner") = graph_fixture(false, 1, 0);
    let original = token_value(&good);
    for field in ["root", "version", "recipe"] {
        let mut forged = original.clone();
        forged["command"]["data"]["page"]["continuation"][field] =
            serde_json::json!("00".repeat(32));
        assert!(
            graph_session(&owner, &requests, false)
                .decode_page_continuation(&token(&forged))
                .is_err()
        );
    }
    let mut forged = original.clone();
    forged["command"]["data"]["symbol"]["id"] = serde_json::json!("00".repeat(32));
    assert!(
        graph_session(&owner, &requests, false)
            .decode_page_continuation(&token(&forged))
            .is_err()
    );
    let mut forged = original.clone();
    forged["command"]["data"]["page"]["continuation"]["query_offset"] = serde_json::json!(0);
    assert!(
        graph_session(&owner, &requests, false)
            .decode_page_continuation(&token(&forged))
            .is_err()
    );
    // Offset and page credit are not authority merely because their JSON is
    // well formed. The producer must reconstruct the exact canonical previous
    // page before returning a successor.
    for (field, value) in [("query_offset", 3), ("query_offset", 4)] {
        let mut forged = original.clone();
        forged["command"]["data"]["page"]["continuation"][field] = serde_json::json!(value);
        let mut fresh = graph_session(&owner, &requests, false);
        let continuation = fresh
            .decode_page_continuation(&token(&forged))
            .expect("canonical but untrusted offset");
        assert!(
            fresh
                .graph_page("graph::source", 2, Some(continuation))
                .is_err()
        );
    }
    let mut forged = original.clone();
    forged["command"]["data"]["page"]["limit"] = serde_json::json!(1);
    let mut fresh = graph_session(&owner, &requests, false);
    let continuation = fresh
        .decode_page_continuation(&token(&forged))
        .expect("canonical but changed credit");
    assert!(
        fresh
            .graph_page("graph::source", 1, Some(continuation))
            .is_err()
    );
    for field in ["scope", "observed", "producer", "context"] {
        let mut forged = original.clone();
        let coverage = forged["certificate"]["claims"]
            .as_array_mut()
            .expect("claims")
            .iter_mut()
            .find(|claim| claim["kind"] == "coverage")
            .expect("scope");
        coverage["data"][field] = serde_json::json!("00".repeat(32));
        assert!(
            graph_session(&owner, &requests, false)
                .decode_page_continuation(&token(&forged))
                .is_err()
        );
    }
}

#[test]
fn plain_graph_selected_address_is_portable_without_becoming_a_canonical_key() {
    let owner = Arc::new(Mutex::new(graph_fixture(true, 1, 0)));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let source = symbol_key("graph::source");
    let mut issuing = graph_session(&owner, &requests, true);
    let first = issuing
        .graph_page_symbol(source, 1, None)
        .expect("selected first");
    let PageTerminal::More(next) = graph_page(&first).terminal else {
        panic!("next")
    };
    let token = issuing
        .encode_query_continuation(next)
        .expect("selected portable proof");
    let mut fresh = graph_session(&owner, &requests, true);
    let continuation = fresh
        .decode_page_continuation(&token)
        .expect("selected import");
    assert!(
        fresh
            .graph_page_symbol(source, 1, Some(continuation))
            .is_ok()
    );
    let mut fresh = graph_session(&owner, &requests, true);
    let continuation = fresh
        .decode_page_continuation(&token)
        .expect("selected import");
    let before = requests.lock().expect("requests").len();
    assert!(
        fresh
            .graph_page("graph::source", 1, Some(continuation))
            .is_err(),
        "canonical and opaque selected addresses are distinct caller contracts"
    );
    assert_eq!(requests.lock().expect("requests").len(), before);
}

// Real owner replies cross the existing typed client admission. The scripted
// readiness replaces only the producer's preparation observation, not rows,
// canonical page proofs or continuation identities.
struct PreparingTransport {
    inner: Transport,
    states: std::collections::VecDeque<backend_library::QueryPreparationState>,
    repeat: bool,
    fault: Option<CommandFailure>,
    sent: Arc<Mutex<Vec<CommandDto>>>,
    budgets: Arc<Mutex<Vec<Duration>>>,
}
impl CommandTransport for PreparingTransport {
    fn request(&mut self, request: CommandDto) -> Result<ReplyDto, ClientError> {
        self.sent.lock().expect("requests").push(request.clone());
        let basis = match &request.command {
            Command::Search(query) => Some(query.basis()),
            Command::GraphQuery(query) => Some(query.page().basis()),
            _ => None,
        };
        if let Some(basis) = basis {
            let failure = self.fault.take().or_else(|| {
                let state = if self.repeat {
                    self.states.front().copied()
                } else {
                    self.states.pop_front()
                }?;
                Some(CommandFailure::QueryPreparation { basis, state })
            });
            if let Some(failure) = failure {
                let owner = self.inner.owner.lock().expect("owner");
                let proof = owner
                    .execute_dto(CommandDto::new(1, Command::Revision))
                    .certificate()
                    .expect("producer proof")
                    .clone();
                let reply = ReplyDto::new(request.request_id, CommandReply::Failed(failure))
                    .with_certificate(proof);
                let wire = serde_json::to_vec(&reply).expect("producer envelope");
                let reply = ReplyDto::decode_with_certificate(&wire, owner.view().capability())
                    .map_err(ClientError::Protocol)?;
                return admit_reply(&request, reply);
            }
        }
        self.inner.request(request)
    }
    fn request_with_budget(
        &mut self,
        request: CommandDto,
        remaining: Duration,
    ) -> Result<ReplyDto, ClientError> {
        self.budgets.lock().expect("budgets").push(remaining);
        self.request(request)
    }
}
fn preparing_session(
    library: &Arc<Mutex<Library>>,
    states: &[backend_library::QueryPreparationState],
    repeat: bool,
    fault: Option<CommandFailure>,
) -> (
    Session,
    Arc<Mutex<Vec<CommandDto>>>,
    Arc<Mutex<Vec<Duration>>>,
) {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let budgets = Arc::new(Mutex::new(Vec::new()));
    (
        Session::from_transport(
            "/private/preparing-owner.sock",
            PreparingTransport {
                inner: Transport {
                    owner: Arc::clone(library),
                    requests: Arc::new(Mutex::new(Vec::new())),
                    graph_ids: None,
                },
                states: states.iter().copied().collect(),
                repeat,
                fault,
                sent: Arc::clone(&sent),
                budgets: Arc::clone(&budgets),
            },
        ),
        sent,
        budgets,
    )
}
#[test]
fn first_search_waits_for_exact_owner_preparation_and_reports_state_transitions() {
    use backend_library::QueryPreparationState::{Preparing, Retiring};
    let library = Arc::new(Mutex::new(owner(1, 7, 0)));
    let (mut session, sent, budgets) =
        preparing_session(&library, &[Retiring, Preparing], false, None);
    let progress = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&progress);
    session.observe_query_preparation(move |event| {
        observed.lock().expect("progress").push(event);
        true
    });
    let reply = session
        .search("Thing", 3)
        .expect("first call returns real rows");
    let CommandReply::Search(page) = reply.reply else {
        panic!("search")
    };
    assert_eq!(page.root.row_count(), 3);
    assert!(page.next.is_some());
    let sent = sent.lock().expect("sent");
    let reads = sent
        .iter()
        .filter(|r| matches!(r.command, Command::Search(_)))
        .collect::<Vec<_>>();
    assert_eq!(reads.len(), 3);
    for pair in reads.windows(2) {
        assert_eq!(pair[0].command, pair[1].command);
        assert_eq!(pair[0].certificate(), pair[1].certificate());
        assert!(pair[0].request_id < pair[1].request_id);
    }
    assert_eq!(
        sent.iter()
            .filter(|r| matches!(r.command, Command::Revision))
            .count(),
        1,
        "preparation never rebinds to a newer revision"
    );
    let budgets = budgets.lock().expect("budgets");
    assert_eq!(
        budgets.len(),
        4,
        "revision selection consumes the same budget"
    );
    assert!(budgets.windows(2).all(|pair| pair[1] < pair[0]));
    let progress = progress.lock().expect("progress");
    assert_eq!(
        progress.iter().map(|p| p.state).collect::<Vec<_>>(),
        [Retiring, Preparing]
    );
    assert!(
        progress
            .iter()
            .all(|p| p.basis == backend_library::ViewRevision::from(page.root.basis().root))
    );
}
#[test]
fn preparation_wait_deadline_and_cancellation_preserve_real_refusal_without_another_send() {
    use backend_library::QueryPreparationState::Preparing;
    let library = Arc::new(Mutex::new(owner(1, 5, 0)));
    for cancel in [false, true] {
        let (mut session, sent, _) = preparing_session(&library, &[Preparing], true, None);
        session.observe_query_preparation(move |_| !cancel);
        let revision = session.revision().expect("revision");
        let result = session.send_query_ready_with_budget(
            Command::Search(Query::new(
                "Thing",
                revision.root,
                QueryLimit::new(2).expect("limit"),
            )),
            Some(revision.certificate),
            Duration::from_millis(5),
        );
        if cancel {
            assert!(matches!(
                result.expect_err("cancelled"),
                ClientError::CommandFailed(CommandFailure::InvalidQuery(_))
            ));
        } else {
            assert!(matches!(
                result.expect("actual readiness").reply,
                CommandReply::Failed(CommandFailure::QueryPreparation {
                    state: Preparing,
                    ..
                })
            ));
        }
        assert_eq!(
            sent.lock()
                .expect("sent")
                .iter()
                .filter(|r| matches!(r.command, Command::Search(_)))
                .count(),
            1
        );
        assert!(
            session.continuations.is_empty(),
            "no empty success or invented continuation"
        );
    }
}
#[test]
fn preparation_wait_never_retries_worker_fault_wrong_basis_or_mutation() {
    let library = Arc::new(Mutex::new(owner(1, 5, 0)));
    // Producer proof context is not canonical row content. Change retained
    // row content to obtain an actual foreign immutable revision.
    let foreign = owner_with_labels(2, 5, 0, &["ForeignCapturedDeclaration"]).revision_root();
    assert_ne!(foreign, library.lock().expect("owner").revision_root());
    for failure in [
        CommandFailure::IncoherentView("authentic worker capture refusal".to_owned()),
        CommandFailure::QueryPreparation {
            basis: foreign.into(),
            state: backend_library::QueryPreparationState::Preparing,
        },
    ] {
        let (mut session, sent, _) = preparing_session(&library, &[], false, Some(failure));
        assert!(session.search("Thing", 2).is_err());
        assert_eq!(
            sent.lock()
                .expect("sent")
                .iter()
                .filter(|r| matches!(r.command, Command::Search(_)))
                .count(),
            1
        );
    }
    let (mut session, sent, _) = preparing_session(&library, &[], false, None);
    let command = Command::Remove {
        package: package_key("absent"),
    };
    assert!(session.send_success(command, None).is_err());
    assert_eq!(sent.lock().expect("sent").len(), 1, "no mutation replay");
}

#[test]
fn preparation_with_same_canonical_rows_and_different_proof_context_remains_same_basis() {
    let first = owner(1, 5, 0);
    let second = owner(2, 5, 0);
    assert_eq!(first.revision_root(), second.revision_root());
    let first_proof = first.execute_dto(CommandDto::new(1, Command::Revision));
    let second_proof = second.execute_dto(CommandDto::new(1, Command::Revision));
    assert_ne!(first_proof.certificate(), second_proof.certificate());
    let library = Arc::new(Mutex::new(first));
    let (mut session, sent, _) = preparing_session(
        &library,
        &[],
        false,
        Some(CommandFailure::QueryPreparation {
            basis: second.revision_root().into(),
            state: backend_library::QueryPreparationState::Preparing,
        }),
    );
    let reply = session
        .search("Thing", 2)
        .expect("same canonical basis prepares normally");
    let CommandReply::Search(page) = reply.reply else {
        panic!("real selected search")
    };
    assert_eq!(page.root.row_count(), 2);
    assert_eq!(
        sent.lock()
            .expect("requests")
            .iter()
            .filter(|r| matches!(r.command, Command::Search(_)))
            .count(),
        2
    );
}
#[test]
fn preparation_wait_reopens_exact_portable_search_page_after_cold_session() {
    use backend_library::QueryPreparationState::Preparing;
    let library = Arc::new(Mutex::new(owner(1, 7, 0)));
    let (mut first, _, _) = preparing_session(&library, &[Preparing], false, None);
    let reply = first
        .search_page("Thing", 2, None)
        .expect("first ready page");
    let CommandReply::Search(page) = reply.reply else {
        panic!("search")
    };
    let next = PageContinuation::from_cursor(page.next.expect("more"));
    let token = first
        .encode_query_continuation(next)
        .expect("real canonical portable proof");
    let expected = {
        let owner = library.lock().expect("owner");
        owner.search(
            &Query::new(
                "Thing",
                owner.revision_root(),
                QueryLimit::new(2).expect("limit"),
            )
            .with_cursor(next.cursor()),
        )
    };
    // Keep the immutable owner; the receiver has no issuer session state.
    let (mut cold, sent, _) = preparing_session(&library, &[Preparing], false, None);
    let reopened = cold
        .decode_page_continuation(&token)
        .expect("cold canonical admission");
    let reply = cold
        .search_page("Thing", 2, Some(reopened))
        .expect("same exact next page");
    let CommandReply::Search(actual) = reply.reply else {
        panic!("search")
    };
    assert_eq!(actual, expected.expect("owner predecessor reconstruction"));
    let sent = sent.lock().expect("sent");
    let reads = sent
        .iter()
        .filter(|r| matches!(r.command, Command::Search(_)))
        .collect::<Vec<_>>();
    assert_eq!(reads.len(), 2);
    assert_eq!(reads[0].command, reads[1].command);
    assert_eq!(reads[0].certificate(), reads[1].certificate());
}

#[test]
fn preparation_wait_does_not_rebind_when_the_owner_changes_after_first_observation() {
    let library = Arc::new(Mutex::new(owner(1, 7, 0)));
    let (mut session, sent, _) = preparing_session(
        &library,
        &[backend_library::QueryPreparationState::Preparing],
        false,
        None,
    );
    let replaced = Arc::clone(&library);
    session.observe_query_preparation(move |_| {
        *replaced.lock().expect("owner") = owner(2, 8, 1);
        true
    });
    assert!(matches!(
        session.search("Thing", 2),
        Err(ClientError::CommandFailed(
            CommandFailure::WrongBasis { .. }
        ))
    ));
    let sent = sent.lock().expect("sent");
    assert_eq!(
        sent.iter()
            .filter(|r| matches!(r.command, Command::Revision))
            .count(),
        1
    );
    let searches = sent
        .iter()
        .filter(|r| matches!(r.command, Command::Search(_)))
        .collect::<Vec<_>>();
    assert_eq!(searches.len(), 2);
    assert_eq!(searches[0].command, searches[1].command);
    assert_eq!(searches[0].certificate(), searches[1].certificate());
}
#[test]
fn preparation_wait_resubmits_only_structured_queries_proven_not_started_and_never_cancel() {
    let library = Arc::new(Mutex::new(owner(1, 7, 0)));
    let (mut session, sent, _) = preparing_session(
        &library,
        &[backend_library::QueryPreparationState::Preparing],
        false,
        None,
    );
    let input = AdmittedGraphQueryInput::new(
        "{ Declaration { coordinate @output } }".to_owned(),
        BTreeMap::new(),
    )
    .expect("graph input");
    let page = session
        .graph_query_admitted(input.clone(), 2, None, false)
        .expect("first preparation completes before graph runs");
    assert_eq!(page.rows.len(), 2);
    let sent = sent.lock().expect("sent");
    let queries = sent
        .iter()
        .filter(|r| matches!(r.command, Command::GraphQuery(_)))
        .collect::<Vec<_>>();
    assert_eq!(queries.len(), 2);
    assert_eq!(queries[0].command, queries[1].command);
    assert_eq!(queries[0].certificate(), queries[1].certificate());
    drop(sent);
    let (mut cancellation, sent, _) = preparing_session(&library, &[], false, None);
    let page = cancellation
        .graph_query_admitted(input, 2, None, true)
        .expect("single control");
    assert_eq!(page.terminal, PageTerminal::Cancelled);
    assert_eq!(
        sent.lock()
            .expect("sent")
            .iter()
            .filter(|r| matches!(r.command, Command::GraphQuery(_)))
            .count(),
        1
    );
}

#[cfg(unix)]
#[test]
fn preparation_wait_observes_the_actual_transport_interrupt_before_resending() {
    struct Interruptible {
        inner: PreparingTransport,
        interrupt: TransportInterrupt,
    }
    impl CommandTransport for Interruptible {
        fn request(&mut self, request: CommandDto) -> Result<ReplyDto, ClientError> {
            self.inner.request(request)
        }
        fn interrupt_handle(&self) -> Option<TransportInterrupt> {
            Some(self.interrupt.clone())
        }
    }
    let library = Arc::new(Mutex::new(owner(1, 5, 0)));
    let sent = Arc::new(Mutex::new(Vec::new()));
    let (stream, _peer) = backend_replication::LocalStream::pair().expect("owned socket pair");
    let interrupt = TransportInterrupt::new(&stream).expect("actual socket interrupt");
    let caller = interrupt.clone();
    let mut session = Session::from_transport(
        "/private/interrupted-preparation.sock",
        Interruptible {
            inner: PreparingTransport {
                inner: Transport {
                    owner: library,
                    requests: Arc::new(Mutex::new(Vec::new())),
                    graph_ids: None,
                },
                states: [backend_library::QueryPreparationState::Preparing].into(),
                repeat: true,
                fault: None,
                sent: Arc::clone(&sent),
                budgets: Arc::new(Mutex::new(Vec::new())),
            },
            interrupt,
        },
    );
    session.observe_query_preparation(move |_| {
        caller.interrupt();
        true
    });
    let started = std::time::Instant::now();
    assert!(matches!(
        session.search("Thing", 2),
        Err(ClientError::CommandFailed(CommandFailure::InvalidQuery(_)))
    ));
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(
        sent.lock()
            .expect("sent")
            .iter()
            .filter(|r| matches!(r.command, Command::Search(_)))
            .count(),
        1,
        "socket cancellation cannot resend the selected read or cancel another owner's worker"
    );
}

#[test]
fn preparation_progress_callback_cannot_renew_the_original_wait_deadline() {
    let library = Arc::new(Mutex::new(owner(1, 5, 0)));
    let (mut session, sent, _) = preparing_session(
        &library,
        &[backend_library::QueryPreparationState::Preparing],
        true,
        None,
    );
    session.observe_query_preparation(|_| {
        std::thread::sleep(Duration::from_millis(20));
        true
    });
    let revision = session.revision().expect("revision");
    let reply = session
        .send_query_ready_with_budget(
            Command::Search(Query::new(
                "Thing",
                revision.root,
                QueryLimit::new(2).expect("limit"),
            )),
            Some(revision.certificate),
            Duration::from_millis(5),
        )
        .expect("preserved readiness");
    assert!(matches!(
        reply.reply,
        CommandReply::Failed(CommandFailure::QueryPreparation { .. })
    ));
    assert_eq!(
        sent.lock()
            .expect("sent")
            .iter()
            .filter(|r| matches!(r.command, Command::Search(_)))
            .count(),
        1
    );
}

#[test]
fn lost_connection_after_preparation_keeps_the_pinned_basis_and_actual_failure_class() {
    struct Disconnecting {
        inner: PreparingTransport,
        reads: usize,
    }
    impl CommandTransport for Disconnecting {
        fn request(&mut self, request: CommandDto) -> Result<ReplyDto, ClientError> {
            if matches!(request.command, Command::Search(_)) {
                self.reads += 1;
                if self.reads == 2 {
                    return Err(ClientError::Disconnected(
                        std::io::ErrorKind::ConnectionReset,
                    ));
                }
            }
            self.inner.request(request)
        }
    }
    let library = Arc::new(Mutex::new(owner(1, 5, 0)));
    let basis = library.lock().expect("owner").revision_root();
    let sent = Arc::new(Mutex::new(Vec::new()));
    let mut session = Session::from_transport(
        "/private/preparing-lost.sock",
        Disconnecting {
            inner: PreparingTransport {
                inner: Transport {
                    owner: library,
                    requests: Arc::new(Mutex::new(Vec::new())),
                    graph_ids: None,
                },
                states: [backend_library::QueryPreparationState::Preparing].into(),
                repeat: false,
                fault: None,
                sent,
                budgets: Arc::new(Mutex::new(Vec::new())),
            },
            reads: 0,
        },
    );
    assert_eq!(
        session
            .search("Thing", 2)
            .expect_err("pinned connection failure"),
        ClientError::QueryPreparationDisconnected {
            basis: basis.into(),
            kind: std::io::ErrorKind::ConnectionReset
        }
    );
}

#[test]
fn lost_first_search_reply_keeps_selected_basis_even_when_the_owner_has_changed() {
    struct LostFirst {
        inner: Transport,
        selected: Arc<Mutex<Option<backend_library::ViewRevision>>>,
        searches: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl CommandTransport for LostFirst {
        fn request(&mut self, request: CommandDto) -> Result<ReplyDto, ClientError> {
            if let Command::Search(query) = &request.command {
                *self.selected.lock().expect("selection") = Some(query.basis());
                self.searches.fetch_add(1, Ordering::AcqRel);
                // The first reply is lost after another real owner is installed;
                // no preparation notification ever reached this client.
                *self.inner.owner.lock().expect("owner") = owner(2, 8, 0);
                return Err(ClientError::Disconnected(
                    std::io::ErrorKind::ConnectionReset,
                ));
            }
            self.inner.request(request)
        }
    }
    let library = Arc::new(Mutex::new(owner(1, 5, 0)));
    let basis: backend_library::ViewRevision =
        library.lock().expect("owner").revision_root().into();
    let selected = Arc::new(Mutex::new(None));
    let searches = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut session = Session::from_transport(
        "/private/lost-first.sock",
        LostFirst {
            inner: Transport {
                owner: Arc::clone(&library),
                requests: Arc::new(Mutex::new(Vec::new())),
                graph_ids: None,
            },
            selected: Arc::clone(&selected),
            searches: Arc::clone(&searches),
        },
    );
    assert_eq!(
        session
            .search_page("Thing", 2, None)
            .expect_err("lost first reply"),
        ClientError::QueryPreparationDisconnected {
            basis,
            kind: std::io::ErrorKind::ConnectionReset
        }
    );
    assert_eq!(*selected.lock().expect("selection"), Some(basis));
    assert_eq!(searches.load(Ordering::Acquire), 1);
    assert!(!basis.matches(library.lock().expect("replacement").revision_root()));
}

#[test]
fn search_selection_and_preparation_share_one_deadline() {
    struct SlowRevision {
        inner: PreparingTransport,
        revision_delay: Duration,
    }
    impl CommandTransport for SlowRevision {
        fn request(&mut self, request: CommandDto) -> Result<ReplyDto, ClientError> {
            self.inner.request(request)
        }
        fn request_with_budget(
            &mut self,
            request: CommandDto,
            remaining: Duration,
        ) -> Result<ReplyDto, ClientError> {
            self.inner.budgets.lock().expect("budgets").push(remaining);
            if matches!(request.command, Command::Revision) {
                std::thread::sleep(self.revision_delay);
            }
            self.inner.request(request)
        }
    }
    // A synchronous transport may finish revision after its assigned budget;
    // the client must then refuse to start Search, not renew thirty seconds.
    for revision_delay in [Duration::from_millis(5), Duration::from_millis(35)] {
        let library = Arc::new(Mutex::new(owner(1, 5, 0)));
        let sent = Arc::new(Mutex::new(Vec::new()));
        let budgets = Arc::new(Mutex::new(Vec::new()));
        let mut session = Session::from_transport(
            "/private/deadline.sock",
            SlowRevision {
                inner: PreparingTransport {
                    inner: Transport {
                        owner: library,
                        requests: Arc::new(Mutex::new(Vec::new())),
                        graph_ids: None,
                    },
                    states: [backend_library::QueryPreparationState::Preparing].into(),
                    repeat: true,
                    fault: None,
                    sent: Arc::clone(&sent),
                    budgets: Arc::clone(&budgets),
                },
                revision_delay,
            },
        );
        let result = session.search_page_with_budget("Thing", 2, None, Duration::from_millis(25));
        let budgets = budgets.lock().expect("budgets");
        if budgets.len() == 1 {
            assert_eq!(
                result.expect_err("selection consumed deadline"),
                ClientError::RemoteDeadlineExceeded
            );
            assert_eq!(budgets.len(), 1);
        } else {
            assert!(matches!(
                result,
                Err(ClientError::CommandFailed(
                    CommandFailure::QueryPreparation { .. }
                ))
            ));
            assert_eq!(budgets.len(), 2);
            assert!(budgets[1] <= budgets[0].saturating_sub(revision_delay));
        }
        let searches = sent
            .lock()
            .expect("requests")
            .iter()
            .filter(|r| matches!(r.command, Command::Search(_)))
            .count();
        assert_eq!(searches, budgets.len() - 1);
    }
}

#[test]
fn caller_context_cancels_preparation_without_sending_shared_worker_control() {
    let library = Arc::new(Mutex::new(owner(1, 7, 0)));
    let (mut session, sent, _) = preparing_session(
        &library,
        &[backend_library::QueryPreparationState::Preparing],
        true,
        None,
    );
    let context = RequestContext::new();
    session
        .set_request_context(Some(context.clone()))
        .expect("bind request context even without a socket");
    session.observe_query_preparation(move |_| {
        context.cancel(RequestCancellationReason::Explicit);
        true
    });
    assert!(matches!(
        session.search("Thing", 2),
        Err(ClientError::RequestCancelled(
            RequestCancellationReason::Explicit
        ))
    ));
    let sent = sent.lock().expect("exact owner command log");
    assert_eq!(
        sent.iter()
            .filter(|request| matches!(request.command, Command::Search(_)))
            .count(),
        1
    );
    assert_eq!(
        sent.len(),
        2,
        "only one Revision and one Search; cancellation sends no owner control"
    );
}

fn delivery_window(issued: u16, delivery: u16) -> backend_library::QueryWindow {
    backend_library::QueryWindow::new(
        QueryLimit::new(issued).expect("issued"),
        QueryLimit::new(delivery).expect("delivery"),
    )
    .expect("window")
}

fn read_window(
    session: &mut Session,
    family: u8,
    delivery: u16,
    continuation: Option<PageContinuation>,
) -> Result<ReplyDto, ClientError> {
    read_window_with_credit(session, family, 4, delivery, continuation)
}

fn read_window_with_credit(
    session: &mut Session,
    family: u8,
    issued: u16,
    delivery: u16,
    continuation: Option<PageContinuation>,
) -> Result<ReplyDto, ClientError> {
    let window = delivery_window(issued, delivery);
    match family {
        0 => session.names_page_window("Thing", window, continuation),
        1 => session.search_page_window("Thing", window, continuation),
        2 => session.graph_page_window("graph::source", window, continuation),
        _ => session.outline_page_window("/p", window, continuation),
    }
}

fn window_snapshot(reply: &ReplyDto) -> &backend_library::ViewSnapshot {
    match &reply.reply {
        CommandReply::Names(page) | CommandReply::Search(page) => page,
        CommandReply::ProjectionPage(page) => &page.snapshot,
        _ => panic!("catalog window reply"),
    }
}

#[test]
fn portable_windows_reimport_consumed_preparation_and_replay_identical_input_at_smaller_delivery() {
    // The injected transport refusal exercises Session consumption and re-import;
    // native producer, strict DTO decoding, certificates and admission remain real.
    struct RefuseWideResume(Transport);
    impl CommandTransport for RefuseWideResume {
        fn request(&mut self, request: CommandDto) -> Result<ReplyDto, ClientError> {
            let refused = match &request.command {
                Command::Name(query) => {
                    query.cursor().is_some() && query.window().delivery().get() > 1
                }
                Command::Search(query) => {
                    query.cursor().is_some() && query.window().delivery().get() > 1
                }
                Command::GraphPage { page, .. } | Command::OutlinePage { page, .. } => {
                    page.continuation().is_some() && page.window().delivery().get() > 1
                }
                _ => false,
            };
            let basis = super::query_continuation::basis(&request.command);
            let reply = self.0.request(request)?;
            if refused {
                Err(ClientError::ResponseTooLarge { basis })
            } else {
                Ok(reply)
            }
        }
    }
    for family in 0..4 {
        let (owner, requests) = if family < 2 {
            fixture()
        } else if family == 3 {
            (
                Arc::new(Mutex::new(owner_with_package(
                    1,
                    7,
                    0,
                    &["Thing"],
                    Some("/p"),
                ))),
                Arc::new(Mutex::new(Vec::new())),
            )
        } else {
            (
                Arc::new(Mutex::new(graph_fixture(false, 1, 7))),
                Arc::new(Mutex::new(Vec::new())),
            )
        };
        let mut issuing = session(&owner, &requests);
        let first = read_window(&mut issuing, family, 4, None).expect("first window");
        let next = PageContinuation::from_cursor(window_snapshot(&first).next.expect("successor"));
        let signed_input = issuing
            .encode_query_continuation(next)
            .expect("portable input");
        let mut resumed = Session::from_transport(
            "/private/window-owner.sock",
            RefuseWideResume(Transport {
                owner: owner.clone(),
                requests: requests.clone(),
                graph_ids: None,
            }),
        );
        let imported = resumed
            .decode_page_continuation(&signed_input)
            .expect("original immutable proof");
        assert_eq!(imported, next);
        assert!(resumed.prepared_query.is_some());
        // MCP may retire a near-spent connection between importing the copied
        // pc4 call and reading its page. Only continuation state crosses that
        // boundary; prepared authority must be rebuilt on the new connection.
        let retained = resumed.take_continuation_state();
        resumed = Session::from_transport(
            "/private/replacement-window-owner.sock",
            RefuseWideResume(Transport {
                owner: owner.clone(),
                requests: requests.clone(),
                graph_ids: None,
            }),
        );
        resumed.restore_continuation_state(retained);
        assert!(resumed.prepared_query.is_none());
        assert!(matches!(
            read_window(&mut resumed, family, 4, Some(imported)),
            Err(ClientError::ResponseTooLarge { basis: Some(_) })
        ));
        assert!(
            resumed.prepared_query.is_none(),
            "the failed read consumed preparation"
        );
        let actual = read_window(&mut resumed, family, 1, Some(imported))
            .expect("same retained input at smaller delivery without another token import");
        let mut replay = session(&owner, &requests);
        let same = replay
            .decode_page_continuation(&signed_input)
            .expect("fresh-session same proof");
        let repeated =
            read_window(&mut replay, family, 1, Some(same)).expect("deterministic retry");
        assert_eq!(window_snapshot(&actual), window_snapshot(&repeated));
        assert_eq!(window_snapshot(&actual).root.row_count(), 1);
        if let Some(cursor) = window_snapshot(&actual).next {
            assert_eq!(cursor.query_offset(), 5);
            assert_eq!(
                cursor.query_position().predecessor(),
                Some(delivery_window(4, 1))
            );
        }
        let mut seen = window_snapshot(&first)
            .root
            .rows()
            .iter()
            .map(|row| row.id)
            .collect::<std::collections::BTreeSet<_>>();
        let mut reply = actual;
        let mut emitted = 4;
        let mut delivery = 2;
        loop {
            let page = window_snapshot(&reply);
            emitted += page.root.row_count();
            for row in page.root.rows() {
                assert!(seen.insert(row.id), "no duplicate frontier row");
            }
            let Some(cursor) = page.next else {
                break;
            };
            assert_eq!(cursor.query_offset(), emitted);
            let token = resumed
                .encode_query_continuation(PageContinuation::from_cursor(cursor))
                .expect("next portable command");
            let mut fresh = session(&owner, &requests);
            let next = fresh
                .decode_page_continuation(&token)
                .expect("fresh authority");
            reply = read_window(&mut fresh, family, delivery, Some(next))
                .expect("next bounded delivery");
            resumed = fresh;
            delivery = if delivery == 2 { 1 } else { 2 };
        }
        assert_eq!(seen.len(), if family < 2 { 37 } else { 7 });
        assert_eq!(emitted as usize, seen.len());
        // A cold imported proof transferred before an exact source/authority
        // change must be re-admitted, not restored as ready-to-send authority.
        let mut stale = session(&owner, &requests);
        let imported = stale
            .decode_page_continuation(&signed_input)
            .expect("old authority");
        let retained = stale.take_continuation_state();
        let mut stale = session(&owner, &requests);
        stale.restore_continuation_state(retained);
        *owner.lock().expect("owner") = if family < 2 {
            self::owner(2, 37, 0)
        } else if family == 3 {
            owner_with_package(2, 7, 0, &["Thing"], Some("/p"))
        } else {
            graph_fixture(false, 2, 7)
        };
        let before = requests.lock().expect("requests").len();
        assert!(matches!(
            read_window(&mut stale, family, 1, Some(imported)),
            Err(ClientError::StaleCursor)
        ));
        assert!(
            requests.lock().expect("requests")[before..]
                .iter()
                .all(|command| matches!(command, Command::Revision)),
            "source change cannot send a page"
        );
    }
}

#[test]
fn portable_window_versions_zero_spans_and_credit_mismatches_refuse() {
    let (owner, requests) = fixture();
    let mut issuing = session(&owner, &requests);
    let first = issuing
        .names_page_window("Thing", delivery_window(4, 2), None)
        .expect("native window");
    let good = issuing
        .encode_query_continuation(PageContinuation::from_cursor(
            page(&first).next.expect("next"),
        ))
        .expect("portable window");
    for (field, value) in [
        ("version", 2),
        ("delivery", 0),
        ("delivery", 5),
        ("issued", 0),
    ] {
        let mut bad = token_value(&good);
        bad["command"]["data"]["cursor"]["query_window"][field] = serde_json::json!(value);
        let before = requests.lock().expect("requests").len();
        assert!(
            session(&owner, &requests)
                .decode_page_continuation(&token(&bad))
                .is_err()
        );
        assert_eq!(
            requests.lock().expect("requests").len(),
            before,
            "malformed window before owner RPC"
        );
    }
    let mut changed = token_value(&good);
    changed["command"]["data"]["window"]["issued"] = serde_json::json!(3);
    assert!(
        session(&owner, &requests)
            .decode_page_continuation(&token(&changed))
            .is_err()
    );
}

#[test]
fn adaptive_session_promotes_issued_active_query_and_keeps_legacy_peer_credit_fixed() {
    let (owner, requests) = fixture();
    let mut issuing = session(&owner, &requests);
    let first = issuing
        .names_page("Thing", 4, None)
        .expect("legacy fixed page");
    let next = PageContinuation::from_cursor(page(&first).next.expect("legacy next"));
    let active = issuing.encode_page_continuation(next);
    let portable = issuing
        .encode_query_continuation(next)
        .expect("legacy command with complete proof");
    assert_eq!(
        issuing
            .decode_page_continuation(&active)
            .expect("legacy active still decodes"),
        next
    );
    let active_reply = issuing
        .names_page_window("Thing", delivery_window(4, 1), Some(next))
        .expect("same-session producer proof supports adaptive delivery");
    let mut fresh = session(&owner, &requests);
    let imported = fresh
        .decode_page_continuation(&portable)
        .expect("portable legacy proof");
    let second = fresh
        .names_page_window("Thing", delivery_window(4, 1), Some(imported))
        .expect("authenticate old recipe before new window recipe");
    assert_eq!(page(&active_reply), page(&second));
    assert_eq!(
        page(&second)
            .next
            .expect("windowed successor")
            .query_position()
            .predecessor(),
        Some(delivery_window(4, 1))
    );
    // An old peer may issue a fixed-width page without a portable next
    // command. Its retained cursor remains usable at the original width,
    // but cannot establish an adaptive query recipe by itself.
    let mut legacy = session(&owner, &requests);
    let first = legacy.names_page("Thing", 4, None).expect("old fixed page");
    let next = PageContinuation::from_cursor(page(&first).next.expect("old next"));
    legacy
        .continuations
        .get_mut(&next.cursor())
        .expect("retained")
        .next_request = None;
    let before = requests.lock().expect("requests").len();
    assert!(matches!(
        legacy.names_page_window("Thing", delivery_window(4, 1), Some(next)),
        Err(ClientError::Protocol(_))
    ));
    assert_eq!(requests.lock().expect("requests").len(), before);
    assert!(legacy.names_page("Thing", 4, Some(next)).is_ok());
}

#[test]
fn retained_window_proofs_refuse_changed_arguments_credit_unknown_cursor_and_stopped_reads() {
    for family in 0..3 {
        let (owner, requests) = if family < 2 {
            fixture()
        } else {
            (
                Arc::new(Mutex::new(graph_fixture(false, 1, 7))),
                Arc::new(Mutex::new(Vec::new())),
            )
        };
        let mut session = session(&owner, &requests);
        let first = read_window(&mut session, family, 4, None).expect("issued page");
        let next =
            PageContinuation::from_cursor(window_snapshot(&first).next.expect("issued next"));
        let before = requests.lock().expect("requests").len();
        let changed = match family {
            0 => session.names_page_window("Other", delivery_window(4, 1), Some(next)),
            1 => session.search_page_window("Other", delivery_window(4, 1), Some(next)),
            _ => {
                session.graph_page_window("graph::other-source", delivery_window(4, 1), Some(next))
            }
        };
        assert!(matches!(changed, Err(ClientError::Protocol(_))));
        assert!(matches!(
            read_window_with_credit(&mut session, family, 5, 1, Some(next)),
            Err(ClientError::Protocol(_))
        ));
        assert!(
            requests.lock().expect("requests")[before..]
                .iter()
                .all(|command| matches!(command, Command::Revision)),
            "changed selection/credit cannot send a page"
        );
        let before = requests.lock().expect("requests").len();
        assert!(matches!(
            read_window(
                &mut session,
                family,
                1,
                Some(PageContinuation::from_cursor(Cursor::new()))
            ),
            Err(ClientError::Protocol(_))
        ));
        assert_eq!(requests.lock().expect("requests").len(), before);
        for expired in [false, true] {
            let context = RequestContext::new();
            session
                .set_request_context(Some(context.clone()))
                .expect("live request context");
            let expected = if expired {
                context
                    .read_deadline(Duration::ZERO)
                    .expect_err("deterministic exhausted budget");
                ClientError::RemoteDeadlineExceeded
            } else {
                context.cancel(RequestCancellationReason::Explicit);
                ClientError::RequestCancelled(RequestCancellationReason::Explicit)
            };
            assert_eq!(
                read_window(&mut session, family, 1, Some(next)),
                Err(expected)
            );
            assert_eq!(
                requests.lock().expect("requests").len(),
                before,
                "stopped read cannot re-admit or send a page"
            );
            session
                .set_request_context(None)
                .expect("clear stopped request");
        }
    }
}

#[test]
fn zero_window_read_budget_refuses_before_any_transport_even_without_request_context() {
    let (owner, requests) = fixture();
    let token = first_token(&owner, &requests, true, 4);
    for prepared in [false, true] {
        let mut resumed = session(&owner, &requests);
        let next = resumed
            .decode_page_continuation(&token)
            .expect("validated input");
        if !prepared {
            resumed.prepared_query = None;
        }
        assert!(resumed.request_context.is_none());
        let before = requests.lock().expect("requests").len();
        assert_eq!(
            resumed.prepare_query_window(delivery_window(4, 1), Some(next), Some(Duration::ZERO)),
            Err(ClientError::RemoteDeadlineExceeded)
        );
        assert_eq!(
            resumed.prepared_query.is_some(),
            prepared,
            "zero budget does not consume the proof"
        );
        let imported = backend_library::decode_command_body(
            &decode_portable_body(&token).expect("canonical input"),
        )
        .expect("strict input command");
        assert_eq!(
            resumed.admit_query_continuation(imported, Some(Duration::ZERO)),
            Err(ClientError::RemoteDeadlineExceeded)
        );
        assert_eq!(
            requests.lock().expect("requests").len(),
            before,
            "default in-process transport ignores budgets, so admission must refuse before calling it"
        );
    }
}

// ---------------------------------------------------------------------------
// A preparing read outlives its connection: the owner retires a connection
// after its frame budget, and a long preparation is exactly what spends it.
// ---------------------------------------------------------------------------

/// Wraps the real-owner preparing transport with the owner's per-connection
/// frame budget. A frame past the budget meets a closed peer, as on the real
/// listener, and a reopened connection starts a fresh budget.
struct RetiringPreparing {
    inner: PreparingTransport,
    cap: usize,
    used: usize,
    advertise: bool,
    /// Frames that reached a retired peer.
    lost: Arc<std::sync::atomic::AtomicUsize>,
    /// Outcomes of successive reopen attempts; an empty script reopens.
    reopen: std::collections::VecDeque<Result<bool, ClientError>>,
    reopened: Arc<std::sync::atomic::AtomicUsize>,
    /// Frames seen so far, across connections.
    seen: usize,
    /// The frame with this 1-based ordinal meets this error once, whatever
    /// the budget.
    fail_once: Option<(usize, ClientError)>,
}
impl RetiringPreparing {
    fn gate(&mut self) -> Result<(), ClientError> {
        self.seen += 1;
        if self
            .fail_once
            .as_ref()
            .is_some_and(|(at, _)| *at == self.seen)
        {
            return Err(self
                .fail_once
                .take()
                .map(|(_, error)| error)
                .expect("scripted"));
        }
        if self.used >= self.cap {
            self.lost.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Err(ClientError::Disconnected(std::io::ErrorKind::BrokenPipe));
        }
        self.used += 1;
        Ok(())
    }
}
impl CommandTransport for RetiringPreparing {
    fn request(&mut self, request: CommandDto) -> Result<ReplyDto, ClientError> {
        self.gate()?;
        self.inner.request(request)
    }
    fn request_with_budget(
        &mut self,
        request: CommandDto,
        remaining: Duration,
    ) -> Result<ReplyDto, ClientError> {
        self.gate()?;
        self.inner.request_with_budget(request, remaining)
    }
    fn frames_remaining(&self) -> Option<usize> {
        self.advertise.then(|| self.cap - self.used)
    }
    fn reconnect_within(
        &mut self,
        _budget: Duration,
        _context: Option<&RequestContext>,
    ) -> Result<bool, ClientError> {
        self.reopened
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        match self.reopen.pop_front().unwrap_or(Ok(true)) {
            Ok(true) => {
                self.used = 0;
                Ok(true)
            }
            other => other,
        }
    }
}

struct RetiringRun {
    session: Session,
    sent: Arc<Mutex<Vec<CommandDto>>>,
    lost: Arc<std::sync::atomic::AtomicUsize>,
    reopened: Arc<std::sync::atomic::AtomicUsize>,
}
fn retiring_session(
    library: &Arc<Mutex<Library>>,
    preparing: usize,
    cap: usize,
    advertise: bool,
    reopen: Vec<Result<bool, ClientError>>,
    fail_once: Option<(usize, ClientError)>,
) -> RetiringRun {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let lost = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let reopened = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let session = Session::from_transport(
        "/private/retiring-owner.sock",
        RetiringPreparing {
            inner: PreparingTransport {
                inner: Transport {
                    owner: Arc::clone(library),
                    requests: Arc::new(Mutex::new(Vec::new())),
                    graph_ids: None,
                },
                states: std::iter::repeat_n(
                    backend_library::QueryPreparationState::Preparing,
                    preparing,
                )
                .collect(),
                repeat: false,
                fault: None,
                sent: Arc::clone(&sent),
                budgets: Arc::new(Mutex::new(Vec::new())),
            },
            cap,
            used: 0,
            advertise,
            lost: Arc::clone(&lost),
            reopen: reopen.into(),
            reopened: Arc::clone(&reopened),
            seen: 0,
            fail_once,
        },
    );
    RetiringRun {
        session,
        sent,
        lost,
        reopened,
    }
}
fn searches(sent: &Arc<Mutex<Vec<CommandDto>>>) -> Vec<CommandDto> {
    sent.lock()
        .expect("sent")
        .iter()
        .filter(|request| matches!(request.command, Command::Search(_)))
        .cloned()
        .collect()
}

/// The reported failure: a search preparing for ~27 s polled 256 times,
/// reached the owner's frame budget, and ended `broken pipe` with the read
/// "not restarted". The same pinned request is asked again on a fresh
/// connection, unchanged, and the search returns real rows.
#[test]
fn a_preparing_search_survives_the_owner_retiring_its_connection() {
    let library = Arc::new(Mutex::new(owner(1, 7, 0)));
    // Revision selection plus four preparing polls and the ready answer need
    // six frames; the budget is three, so the connection is retired mid-wait.
    let mut run = retiring_session(&library, 4, 3, false, vec![], None);
    let reply = run
        .session
        .search("Thing", 3)
        .expect("the retirement is survived, not reported");
    let CommandReply::Search(page) = reply.reply else {
        panic!("real rows, not a failure")
    };
    assert_eq!(page.root.row_count(), 3);
    let lost = run.lost.load(std::sync::atomic::Ordering::Relaxed);
    assert!(lost >= 1, "the budget really was exhausted: {lost}");
    assert_eq!(
        run.reopened.load(std::sync::atomic::Ordering::Relaxed),
        lost,
        "every lost frame was answered by exactly one reopen"
    );
    let reads = searches(&run.sent);
    assert_eq!(reads.len(), 5, "four preparing answers, then the rows");
    for pair in reads.windows(2) {
        assert_eq!(pair[0].command, pair[1].command, "same pinned query");
        assert_eq!(pair[0].certificate(), pair[1].certificate());
    }
    assert_eq!(
        run.sent
            .lock()
            .expect("sent")
            .iter()
            .filter(|request| matches!(request.command, Command::Revision))
            .count(),
        1,
        "the revision is never refreshed"
    );
}

/// With the budget known the connection is replaced before it is spent, so no
/// poll is ever written to a retired peer.
#[test]
fn a_known_frame_budget_replaces_the_connection_before_a_poll_is_lost() {
    let library = Arc::new(Mutex::new(owner(1, 7, 0)));
    let mut run = retiring_session(&library, 4, 3, true, vec![], None);
    run.session
        .search("Thing", 3)
        .expect("the search completes across proactive reopens");
    assert_eq!(run.lost.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert!(run.reopened.load(std::sync::atomic::Ordering::Relaxed) >= 1);
    assert_eq!(searches(&run.sent).len(), 5);
}

/// Polls are paced with a growing wait, not a fixed 100 ms: five preparing
/// answers cost at least 100 + 200 + 400 + 500 + 500 ms, where the fixed pace
/// would have spent 500 ms and five times the frames per second.
#[test]
fn preparation_polls_back_off_instead_of_burning_frames() {
    let library = Arc::new(Mutex::new(owner(1, 7, 0)));
    let mut run = retiring_session(&library, 5, 100, false, vec![], None);
    let started = std::time::Instant::now();
    run.session.search("Thing", 3).expect("search");
    assert!(
        started.elapsed() >= Duration::from_millis(1700),
        "poll cadence grows toward its ceiling: {:?}",
        started.elapsed()
    );
}

/// A read timeout is a live owner that did not answer; the same pinned read is
/// asked again on a fresh connection to the same listener.
#[test]
fn a_slow_owner_during_preparation_is_asked_again_on_a_fresh_connection() {
    let library = Arc::new(Mutex::new(owner(1, 7, 0)));
    // Frame 1 is the revision read; frame 2 is the first pinned poll.
    let mut run = retiring_session(
        &library,
        1,
        100,
        false,
        vec![],
        Some((2, ClientError::Disconnected(std::io::ErrorKind::WouldBlock))),
    );
    let reply = run
        .session
        .search("Thing", 3)
        .expect("a timed-out poll is asked again, not reported");
    assert!(matches!(reply.reply, CommandReply::Search(_)));
    assert_eq!(run.reopened.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert_eq!(
        searches(&run.sent).len(),
        2,
        "the poll that timed out never reached the owner"
    );
}

/// A replacement that is lost too (a dying owner's last accept) is waited out;
/// no owner at all, or a transport that cannot reopen, keeps the original loss.
#[test]
fn reopening_waits_out_lost_replacements_and_keeps_the_loss_when_nothing_answers() {
    let library = Arc::new(Mutex::new(owner(1, 7, 0)));
    let reset = || ClientError::Disconnected(std::io::ErrorKind::ConnectionReset);
    let mut run = retiring_session(
        &library,
        1,
        2,
        false,
        vec![Err(reset()), Err(reset()), Ok(true)],
        None,
    );
    run.session
        .search("Thing", 3)
        .expect("lost replacements are retried within the deadline");
    assert!(run.reopened.load(std::sync::atomic::Ordering::Relaxed) >= 3);

    for reopen in [
        Err(ClientError::Disconnected(
            std::io::ErrorKind::ConnectionRefused,
        )),
        Ok(false),
    ] {
        let mut run = retiring_session(&library, 3, 1, false, vec![reopen], None);
        let error = run
            .session
            .search("Thing", 3)
            .expect_err("nothing to ask again");
        assert!(
            matches!(
                error,
                ClientError::QueryPreparationDisconnected {
                    kind: std::io::ErrorKind::BrokenPipe,
                    ..
                }
            ),
            "{error:?}"
        );
        assert_eq!(
            run.reopened.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "one attempt, no spin"
        );
    }
}

#[test]
fn portable_outline_reopens_exact_native_proof_and_rejects_credit_package_and_scope_changes() {
    let owner = Arc::new(Mutex::new(owner_with_package(
        1,
        7,
        0,
        &["Thing"],
        Some("/p"),
    )));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let mut issuing = session(&owner, &requests);
    let first = issuing
        .outline_page("/p", 3, None)
        .expect("native outline page");
    let CommandReply::ProjectionPage(first) = first.reply else {
        panic!("projection");
    };
    assert_eq!(
        first.outline_order.as_ref().expect("first order").start(),
        0
    );
    assert!(
        first
            .snapshot
            .root
            .rows()
            .iter()
            .all(|row| row.score.is_none())
    );
    let next = PageContinuation::from_cursor(first.snapshot.next.expect("more"));
    let token = issuing
        .encode_query_continuation(next)
        .expect("portable native outline proof");
    let mut fresh = session(&owner, &requests);
    let imported = fresh
        .decode_page_continuation(&token)
        .expect("fresh owner admits proof");
    let resumed = fresh
        .outline_page("/p", 3, Some(imported))
        .expect("exact successor");
    let CommandReply::ProjectionPage(resumed) = resumed.reply else {
        panic!("projection");
    };
    assert_eq!(resumed.snapshot.root.row_count(), 3);
    assert_eq!(
        resumed
            .outline_order
            .as_ref()
            .expect("resumed order")
            .start(),
        3
    );
    assert_eq!(
        resumed
            .snapshot
            .next
            .expect("last page follows")
            .query_offset(),
        6
    );
    for (path, credit) in [("/other", 3), ("/p", 1)] {
        let mut fresh = session(&owner, &requests);
        let imported = fresh.decode_page_continuation(&token).expect("same proof");
        let before = requests.lock().expect("requests").len();
        assert!(fresh.outline_page(path, credit, Some(imported)).is_err());
        assert_eq!(
            requests.lock().expect("requests").len(),
            before,
            "refused before sending"
        );
    }
    let mut changed = token_value(&token);
    changed["command"]["data"]["page"]["limit"] = serde_json::json!(1);
    assert!(
        session(&owner, &requests)
            .decode_page_continuation(&self::token(&changed))
            .is_err(),
        "native recipe binds exact credit"
    );
    let mut wrong_predecessor = token_value(&token);
    wrong_predecessor["command"]["data"]["page"]["continuation"]["query_offset"] =
        serde_json::json!(6);
    assert!(
        session(&owner, &requests)
            .decode_page_continuation(&self::token(&wrong_predecessor))
            .is_err(),
        "the same predecessor row proof cannot claim another ordered start"
    );
    *owner.lock().expect("owner") = owner_with_package(2, 7, 0, &["Thing"], Some("/p"));
    assert!(
        session(&owner, &requests)
            .decode_page_continuation(&token)
            .is_err(),
        "foreign issuing scope"
    );
}

#[test]
fn compact_outline_witness_rehydrates_cold_and_preserves_complete_page_union() {
    let owner = Arc::new(Mutex::new(owner_with_package(
        1,
        7,
        0,
        &["Thing"],
        Some("/p"),
    )));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let mut issuing = session(&owner, &requests);
    let first = issuing
        .outline_page("/p", 3, None)
        .expect("first outline page");
    let mut emitted = page(&first)
        .root
        .rows()
        .iter()
        .map(|row| row.id)
        .collect::<std::collections::BTreeSet<_>>();
    let first_cursor = page(&first).next.expect("first successor");
    let token = issuing
        .encode_outline_query_continuation(PageContinuation::from_cursor(first_cursor))
        .expect("compact owner witness");
    assert!(token.starts_with("pc5-"));
    let body = decode_portable_body(&token).expect("bounded expanded proof");
    assert!(
        backend_library::decode_command_body(&body).is_err(),
        "digest-only outline witness never enters the standalone decoder"
    );
    let decoded = backend_library::decode_command_body_for_owner(
        &body,
        owner.lock().expect("owner").cursor(),
    )
    .expect("owner-only witness decoder");
    let Command::OutlinePage {
        page: request_page, ..
    } = decoded.command
    else {
        panic!("outline page command");
    };
    assert!(request_page.outline_witness().is_some());
    assert!(request_page.continuation().is_none());

    let mut restarted = session(&owner, &requests);
    let second = restarted
        .outline_page_from_query_token("/p", backend_library::QueryWindow::fixed(
            QueryLimit::new(3).expect("credit"),
        ), &token)
        .expect("cold owner rehydrates predecessor and resumes");
    let second_page = page(&second);
    emitted.extend(second_page.root.rows().iter().map(|row| row.id));
    let second_cursor = second_page.next.expect("third page remains");
    let next = restarted
        .encode_outline_query_continuation(PageContinuation::from_cursor(second_cursor))
        .expect("next compact witness");
    let mut cold_again = session(&owner, &requests);
    let third = cold_again
        .outline_page_from_query_token("/p", backend_library::QueryWindow::fixed(
            QueryLimit::new(3).expect("credit"),
        ), &next)
        .expect("second cold owner rehydrates predecessor");
    assert_eq!(page(&third).next, None);
    emitted.extend(page(&third).root.rows().iter().map(|row| row.id));
    let expected = owner
        .lock()
        .expect("owner")
        .view()
        .rows()
        .iter()
        .filter_map(|row| matches!(row.id, RowId::Symbol(_)).then_some(row.id))
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(emitted, expected, "no row is lost or repeated across the union");
}

#[test]
fn compact_outline_witness_rejects_wrong_scope_credit_and_rehydrated_root_before_reply() {
    let owner = Arc::new(Mutex::new(owner_with_package(
        1,
        7,
        0,
        &["Thing"],
        Some("/p"),
    )));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let mut issuing = session(&owner, &requests);
    let first = issuing.outline_page("/p", 3, None).expect("first page");
    let cursor = page(&first).next.expect("successor");
    let token = issuing
        .encode_outline_query_continuation(PageContinuation::from_cursor(cursor))
        .expect("compact witness");
    let window = backend_library::QueryWindow::fixed(QueryLimit::new(3).expect("credit"));

    for (path, credit) in [("/other", 3), ("/p", 2)] {
        let count = requests.lock().expect("requests").len();
        let mut cold = session(&owner, &requests);
        let selected = backend_library::QueryWindow::fixed(
            QueryLimit::new(credit).expect("selected credit"),
        );
        assert!(cold
            .outline_page_from_query_token(path, selected, &token)
            .is_err());
        assert_eq!(requests.lock().expect("requests").len(), count);
    }

    let compact_body = decode_portable_body(&token).expect("portable body");
    let owner_cursor = owner.lock().expect("owner").cursor();
    let imported = backend_library::decode_command_body_for_owner(&compact_body, owner_cursor)
        .expect("owner-decoded witness");
    let certificate = imported.certificate().expect("certificate");
    let Command::OutlinePage { package, page } = imported.command else {
        panic!("outline command");
    };
    let witness = page.outline_witness().expect("untrusted witness");
    let old_root = backend_library::encode_id(&witness.root_commitment());
    let forged_root = [0xA5; 32];
    let forged_root_id = backend_library::encode_id(&forged_root);
    let forged_witness = OutlineCursorWitness::new_untrusted(
        witness.recipe(),
        witness.version(),
        witness.branch(),
        witness.log(),
        witness.schema(),
        forged_root,
        witness.sequence(),
        witness.query_position().offset(),
        witness.query_position().predecessor(),
    )
    .expect("well-formed forged digest position");
    let claims = certificate
        .claims
        .iter()
        .cloned()
        .map(|claim| match claim {
            WireClaim::RootCommitment {
                schema: WireSchema::ViewRelation,
                id,
            } if id == old_root => WireClaim::RootCommitment {
                schema: WireSchema::ViewRelation,
                id: forged_root_id.clone(),
            },
            WireClaim::Cursor {
                recipe,
                version,
                branch,
                log,
                schema,
                root,
                sequence,
            } if root == old_root => WireClaim::Cursor {
                recipe,
                version,
                branch,
                log,
                schema,
                root: forged_root_id.clone(),
                sequence,
            },
            other => other,
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let forged = CommandDto::new(
        1,
        Command::OutlinePage {
            package,
            page: page.with_outline_witness(forged_witness),
        },
    )
    .with_certificate(WireCertificate::from_claims(claims));
    let forged_body = backend_library::encode_binary_certificate_command(&forged)
        .expect("bounded forged owner witness");
    let forged_token = encode_portable_proof(&forged_body, "pc5-").expect("compressed witness");
    let count = requests.lock().expect("requests").len();
    let mut cold = session(&owner, &requests);
    assert!(cold
        .outline_page_from_query_token("/p", window, &forged_token)
        .is_err(), "matching digest and cursor claims still cannot invent predecessor rows");
    assert_eq!(requests.lock().expect("requests").len(), count + 1);

    *owner.lock().expect("owner") = owner_with_package(2, 7, 0, &["Thing"], Some("/p"));
    let count = requests.lock().expect("requests").len();
    let mut stale = session(&owner, &requests);
    assert!(stale
        .outline_page_from_query_token("/p", window, &token)
        .is_err());
    assert_eq!(requests.lock().expect("requests").len(), count);
}

// This adversarial witness deliberately discloses the actual owner relation.
// Revision is commitment-only; its bounded certificate must never supply it.
fn canonical_member_owner_claim(library: &Library) -> WireClaim {
    let root = library.view();
    let id = encode_id(root.root().as_bytes());
    let canonical = root
        .canonical_relation_bytes()
        .expect("actual canonical owner relation");
    assert_eq!(
        backend_library::admit_root_bytes::<backend_library::ViewRelation>(&id, &canonical)
            .expect("independently rehashed owner relation"),
        library.revision_root()
    );
    WireClaim::Root {
        schema: WireSchema::ViewRelation,
        id,
        canonical: canonical.into_boxed_slice(),
    }
}

// Emulate the real service's bounded owner revision, while retaining the
// canonical predecessor PAGE required by the strict continuation decoder.
struct BoundedMembersTransport {
    inner: Transport,
    predecessor: Option<(Cursor, WireClaim, u64, u16)>,
}
impl BoundedMembersTransport {
    fn new(inner: Transport) -> Self {
        Self {
            inner,
            predecessor: None,
        }
    }

    fn admit_request_proof(&self, request: &CommandDto) -> Result<(), ClientError> {
        let owner_root = self.inner.owner.lock().expect("owner").revision_root();
        let owner_id = encode_id(owner_root.as_bytes());
        let proof = request
            .certificate()
            .ok_or_else(|| ClientError::Protocol("missing owner commitment".to_owned()))?;
        if !proof.claims.iter().any(|claim| {
            matches!(claim,
            WireClaim::RootCommitment { schema: WireSchema::ViewRelation, id }
                if id == &owner_id)
        }) {
            return Err(ClientError::Protocol(
                "missing exact owner commitment".to_owned(),
            ));
        }
        let roots = proof
            .claims
            .iter()
            .filter(|claim| {
                matches!(
                    claim,
                    WireClaim::Root {
                        schema: WireSchema::ViewRelation,
                        ..
                    }
                )
            })
            .collect::<Vec<_>>();
        if roots.iter().any(|claim| {
            matches!(claim,
            WireClaim::Root { id, .. } if id == &owner_id)
        }) {
            return Err(ClientError::Protocol(
                "whole owner relation disclosed".to_owned(),
            ));
        }
        match &request.command {
            Command::MembersPage { page, .. } if page.continuation().is_some() => {
                let continuation = page.continuation().ok_or(ClientError::StaleCursor)?;
                let (cursor, claim, rows, credit) =
                    self.predecessor.as_ref().ok_or(ClientError::StaleCursor)?;
                if continuation.cursor() != *cursor
                    || *credit != page.limit().get()
                    || *rows > u64::from(*credit)
                    || roots.as_slice() != [claim]
                    || !matches!(claim, WireClaim::Root { id, .. }
                        if id == &encode_id(cursor.root().as_bytes()))
                {
                    return Err(ClientError::Protocol(
                        "not the exact bounded predecessor page".to_owned(),
                    ));
                }
            }
            _ if !roots.is_empty() => {
                return Err(ClientError::Protocol(
                    "initial member/document request has a canonical relation".to_owned(),
                ));
            }
            _ => {}
        }
        Ok(())
    }
}
impl CommandTransport for BoundedMembersTransport {
    fn request(&mut self, request: CommandDto) -> Result<ReplyDto, ClientError> {
        if matches!(
            &request.command,
            Command::Document(_) | Command::MembersPage { .. }
        ) {
            // A real canonical owner witness must still be rejected. This
            // fault injection would catch a leak even on continued pages.
            let owner_claim = {
                let library = self.inner.owner.lock().expect("owner");
                canonical_member_owner_claim(&library)
            };
            let leaked = request.clone().with_certificate(
                request
                    .certificate()
                    .expect("request proof")
                    .clone()
                    .with_claim(owner_claim),
            );
            assert!(
                matches!(self.admit_request_proof(&leaked), Err(ClientError::Protocol(message))
                if message == "whole owner relation disclosed")
            );
            self.admit_request_proof(&request)?;
            let bytes =
                backend_library::encode_command_body(&request).map_err(ClientError::Protocol)?;
            let admitted =
                backend_library::decode_command_body(&bytes).map_err(ClientError::Protocol)?;
            assert_eq!(
                admitted, request,
                "actual document/member request wire admission"
            );
        }
        let member_credit = match &request.command {
            Command::MembersPage { page, .. } => Some(page.limit().get()),
            _ => None,
        };
        let revision = matches!(&request.command, Command::Revision);
        let mut reply = self.inner.request(request)?;
        if revision {
            let root = self.inner.owner.lock().expect("owner").revision_root();
            let certificate = reply.certificate().expect("owner revision proof");
            let claims = certificate
                .claims
                .iter()
                .filter(|claim| {
                    !matches!(
                        claim,
                        WireClaim::Root {
                            schema: WireSchema::ViewRelation,
                            ..
                        } | WireClaim::RootCommitment {
                            schema: WireSchema::ViewRelation,
                            ..
                        }
                    )
                })
                .cloned()
                .collect::<Vec<_>>();
            let mut proof = WireCertificate::new();
            for claim in claims {
                proof = proof.with_claim(claim);
            }
            reply = reply.with_certificate(proof.with_claim(WireClaim::RootCommitment {
                schema: WireSchema::ViewRelation,
                id: encode_id(root.as_bytes()),
            }));
            let bytes = serde_json::to_vec(&reply).expect("bounded owner revision wire");
            let library = self.inner.owner.lock().expect("owner");
            reply = ReplyDto::decode_with_certificate(&bytes, library.view().capability())
                .map_err(ClientError::Protocol)?;
        }
        if let Some(credit) = member_credit {
            let CommandReply::ProjectionPage(page) = &reply.reply else {
                return Err(ClientError::Protocol("missing member page".to_owned()));
            };
            assert!(page.snapshot.root.row_count() <= u64::from(credit));
            self.predecessor = match page.terminal {
                PageTerminal::More(next) => {
                    assert_eq!(next.cursor().root(), page.snapshot.root.root());
                    let claim = WireClaim::Root {
                        schema: WireSchema::ViewRelation,
                        id: encode_id(page.snapshot.root.root().as_bytes()),
                        canonical: page
                            .snapshot
                            .root
                            .canonical_relation_bytes()
                            .expect("bounded page preimage")
                            .into_boxed_slice(),
                    };
                    assert!(
                        reply
                            .certificate()
                            .expect("page certificate")
                            .claims
                            .contains(&claim)
                    );
                    Some((next.cursor(), claim, page.snapshot.root.row_count(), credit))
                }
                PageTerminal::Complete | PageTerminal::Cancelled => None,
            };
        }
        Ok(reply)
    }
}

fn direct_member_fixture() -> (Arc<Mutex<Library>>, Arc<Mutex<Vec<Command>>>, SymbolKey) {
    let base = owner_with_package(1, 406, 0, &["pkg::models.py:994::Document"], Some("pkg"));
    let center = symbol_key("pkg::Thing000");
    let mut rows = base.view().rows().to_vec();
    for row in &mut rows {
        if matches!(row.id, RowId::Symbol(symbol) if symbol != center) {
            row.parent = Some(center);
        }
    }
    let root = ViewRoot::new_checked(
        base.view().recipe(),
        base.view().basis(),
        base.view().frontier(),
        rows,
        base.view().coverage().to_vec(),
        base.view().capability().expect("same fixture authority"),
    )
    .expect("exact parents");
    let library = Library::from_view(root.clone(), Cursor::for_view_root(&root)).expect("catalog");
    let owner = Arc::new(Mutex::new(library));
    let requests = Arc::new(Mutex::new(Vec::new()));
    (owner, requests, center)
}

#[test]
fn direct_member_session_pages_pin_selected_owner_and_source_without_outline_requests() {
    let (owner, requests, center) = direct_member_fixture();
    {
        let library = owner.lock().expect("owner");
        let revision = library.execute_dto(CommandDto::new(1, Command::Revision));
        let claims = &revision.certificate().expect("revision certificate").claims;
        let owner_id = encode_id(library.revision_root().as_bytes());
        assert!(claims.iter().any(|claim| matches!(claim,
            WireClaim::RootCommitment { schema: WireSchema::ViewRelation, id }
                if id == &owner_id)));
        assert!(
            !claims.iter().any(|claim| matches!(
                claim,
                WireClaim::Root {
                    schema: WireSchema::ViewRelation,
                    ..
                }
            )),
            "revision must not disclose canonical owner relations"
        );
        let WireClaim::Root { id, canonical, .. } = canonical_member_owner_claim(&library) else {
            panic!("canonical owner relation witness")
        };
        let wrong_id = encode_id(view_state_root(&[]).as_bytes());
        assert_ne!(id, wrong_id, "the fixture has a nonempty owner relation");
        assert!(
            backend_library::admit_root_bytes::<backend_library::ViewRelation>(
                &wrong_id, &canonical
            )
            .is_err(),
            "canonical bytes cannot certify another owner root"
        );
        let mut changed = canonical.into_vec();
        *changed.first_mut().expect("canonical relation bytes") ^= 1;
        assert!(
            backend_library::admit_root_bytes::<backend_library::ViewRelation>(&id, &changed)
                .is_err(),
            "changed bytes cannot certify the selected owner root"
        );
    }
    let mut client = Session::from_transport(
        "/private/test-owner.sock",
        BoundedMembersTransport::new(Transport {
            owner: owner.clone(),
            requests: requests.clone(),
            graph_ids: None,
        }),
    );
    let reply = client.document_symbol(center).expect("selected document");
    let CommandReply::Document(document) = reply.reply else {
        panic!("document");
    };
    let source = document.source_basis().expect("exact selected source");
    let row = owner
        .lock()
        .expect("owner")
        .view()
        .row_ref(RowId::Symbol(center))
        .expect("selected owner")
        .clone();
    let mut changed_sources = [source; 5];
    changed_sources[0].root = view_state_root(&[]);
    changed_sources[1].object = object_version(b"other member source");
    changed_sources[2].branch = backend_library::branch_key("other member branch");
    changed_sources[3].log = backend_library::log_key("other member log");
    changed_sources[4].schema = source.schema + 1;
    for changed in changed_sources {
        assert_ne!(changed, source);
        let selection = backend_library::DocumentSelection::from_row(&row, changed)
            .expect("independently consistent owner selection");
        let wrong = backend_library::Document::new(center, changed.root, vec![])
            .with_source_basis(changed)
            .with_selection(selection)
            .expect("consistent document");
        let before = requests
            .lock()
            .expect("requests")
            .iter()
            .filter(|command| matches!(command, Command::MembersPage { .. }))
            .count();
        assert!(client.members_page(&wrong, 200, None).is_err());
        assert_eq!(
            before,
            requests
                .lock()
                .expect("requests")
                .iter()
                .filter(|command| matches!(command, Command::MembersPage { .. }))
                .count(),
            "changed source must refuse before member request"
        );
        assert!(client.continuations.is_empty());
    }
    let mut wrong_owner = document.clone();
    wrong_owner.symbol = symbol_key("foreign selected owner");
    assert!(client.members_page(&wrong_owner, 200, None).is_err());
    let mut missing_selection = document.clone();
    missing_selection.selection = None;
    assert!(client.members_page(&missing_selection, 200, None).is_err());
    assert!(
        !requests
            .lock()
            .expect("requests")
            .iter()
            .any(|command| matches!(command, Command::MembersPage { .. }))
    );
    let mut continuation = None;
    let mut seen = std::collections::BTreeSet::new();
    loop {
        let reply = client
            .members_page(&document, 200, continuation)
            .expect("admitted member wire");
        assert!(admit_member_page_source(&reply, source).is_ok());
        for changed in changed_sources {
            assert!(matches!(
                admit_member_page_source(&reply, changed),
                Err(ClientError::StaleCursor)
            ));
        }
        let CommandReply::ProjectionPage(page) = reply.reply else {
            panic!("member page");
        };
        for row in page.snapshot.root.rows() {
            assert!(seen.insert(row.id));
        }
        match page.terminal {
            PageTerminal::More(next) => continuation = Some(next),
            PageTerminal::Complete => break,
            PageTerminal::Cancelled => panic!("not a cancelled fixture"),
        }
    }
    assert_eq!(seen.len(), 406);
    let requests = requests.lock().expect("requests");
    let members = requests
        .iter()
        .filter_map(|command| match command {
            Command::MembersPage { owner, page } => Some((owner, page)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(members.len(), 3);
    assert!(
        members
            .iter()
            .all(|(owner, page)| owner.symbol().is_selected()
                && owner.symbol().claimed_bytes() == *center.as_bytes()
                && owner.source_basis().is_none()
                && page.limit().get() == 200)
    );
    assert!(
        !requests
            .iter()
            .any(|command| matches!(command, Command::Outline(_) | Command::OutlinePage { .. }))
    );
}

struct PublishedNamesTransport {
    inner: Transport,
    full_ready: Arc<std::sync::atomic::AtomicBool>,
}
impl CommandTransport for PublishedNamesTransport {
    fn request(&mut self, request: CommandDto) -> Result<ReplyDto, ClientError> {
        let request = match &request.command {
            Command::Search(query) if query.cursor().is_none() => {
                let query = backend_library::PublishedNameSearchQuery::admit(query.clone())
                    .map_err(|_| {
                        ClientError::Protocol("stage fixture accepts names only".to_owned())
                    })?;
                CommandDto::new(request.request_id, Command::SearchPublishedNames(query))
                    .with_certificate(request.certificate().expect("initial owner proof").clone())
            }
            _ => request,
        };
        let mut reply = self.inner.request(request)?;
        if self.full_ready.load(std::sync::atomic::Ordering::Acquire)
            && let CommandReply::PublishedNameSearch(page) = &mut reply.reply
        {
            page.readiness = backend_library::PublishedSearchReadiness {
                full_search: backend_library::FullSearchReadiness::Ready,
                semantic: backend_library::PublishedSemanticReadiness::NotQueried,
            };
        }
        Ok(reply)
    }
}

#[test]
fn copied_search_stage_continuation_never_promotes_after_full_projection_becomes_ready() {
    let owner = Arc::new(Mutex::new(owner(1, 7, 0)));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let ready = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let make = || {
        Session::from_transport(
            "/private/published-names.sock",
            PublishedNamesTransport {
                inner: Transport {
                    owner: Arc::clone(&owner),
                    requests: Arc::clone(&requests),
                    graph_ids: None,
                },
                full_ready: Arc::clone(&ready),
            },
        )
    };
    let mut issuing = make();
    let first = issuing.search("Thing", 3).expect("useful name phase");
    let CommandReply::PublishedNameSearch(first) = first.reply else {
        panic!("explicit phase");
    };
    assert_eq!(
        first.readiness.full_search,
        backend_library::FullSearchReadiness::Preparing
    );
    let mut seen = first
        .snapshot
        .root
        .rows()
        .iter()
        .map(|row| row.id)
        .collect::<std::collections::BTreeSet<_>>();
    let continuation = PageContinuation::from_cursor(first.snapshot.next.expect("next stage page"));
    let token = issuing
        .encode_query_continuation(continuation)
        .expect("portable stage command");
    ready.store(true, std::sync::atomic::Ordering::Release);
    let mut fresh = make();
    let continuation = fresh
        .decode_page_continuation(&token)
        .expect("same source/owner");
    let mut reply = fresh
        .search_page("Thing", 3, Some(continuation))
        .expect("same phase after readiness");
    loop {
        let CommandReply::PublishedNameSearch(page) = reply.reply else {
            panic!("never full-search promotion");
        };
        assert_eq!(
            page.readiness.full_search,
            backend_library::FullSearchReadiness::Ready
        );
        for row in page.snapshot.root.rows() {
            assert!(seen.insert(row.id));
        }
        let Some(next) = page.snapshot.next else {
            break;
        };
        reply = fresh
            .search_page("Thing", 3, Some(PageContinuation::from_cursor(next)))
            .expect("same stage");
    }
    assert_eq!(seen.len(), 7);
    assert!(
        requests
            .lock()
            .expect("commands")
            .iter()
            .all(|command| matches!(
                command,
                Command::Revision | Command::SearchPublishedNames(_)
            ))
    );
    assert_eq!(
        requests
            .lock()
            .expect("commands")
            .iter()
            .filter(|command| matches!(command, Command::SearchPublishedNames(_)))
            .count(),
        3,
        "all seven rows require three name-stage pages under fixed credit three",
    );
    let mut changed_credit = make();
    let continuation = changed_credit
        .decode_page_continuation(&token)
        .expect("exact authenticated stage before changed-credit control");
    assert!(
        changed_credit
            .search_page("Thing", 2, Some(continuation))
            .is_err(),
        "a copied stage cursor cannot change its issued credit"
    );
    let mut wrong = make();
    let continuation = wrong
        .decode_page_continuation(&token)
        .expect("authenticated token");
    assert!(wrong.search_page("Other", 3, Some(continuation)).is_err());
    *owner.lock().expect("replace source") = owner_with_label(1, 8, 0, "Thing repeated");
    let mut stale = make();
    assert!(
        stale.decode_page_continuation(&token).is_err(),
        "no stale root or coordinate re-resolution"
    );
}

// Fault injection after the inner transport's strict reply admission exercises
// the Session's last source guard independently of wire rejection. Every
// replacement view and offered cursor is constructed through public typed APIs.
struct ChangedMemberReplySource {
    inner: BoundedMembersTransport,
    source: Basis,
    offered: Arc<Mutex<Option<PageContinuation>>>,
}
impl CommandTransport for ChangedMemberReplySource {
    fn request(&mut self, request: CommandDto) -> Result<ReplyDto, ClientError> {
        let members = matches!(&request.command, Command::MembersPage { .. });
        let mut reply = self.inner.request(request)?;
        if members {
            let CommandReply::ProjectionPage(page) = &mut reply.reply else {
                panic!("actual member page");
            };
            assert!(
                matches!(page.terminal, PageTerminal::More(_)),
                "real fixture offers a successor"
            );
            let rows = page
                .snapshot
                .root
                .rows()
                .iter()
                .cloned()
                .map(|mut row| {
                    row.basis = self.source;
                    row
                })
                .collect();
            let frontier = Frontier::new(
                self.source.branch,
                self.source.log,
                self.source.schema,
                self.source.root,
                page.snapshot.root.frontier().sequence,
            );
            let changed = ViewRoot::new_incomplete(
                page.snapshot.root.recipe(),
                self.source,
                frontier,
                rows,
                vec![],
            )
            .expect("consistent source-bound fault view");
            let next = PageContinuation::from_cursor(Cursor::for_view_root(&changed));
            page.snapshot.root = changed;
            page.snapshot.next = Some(next.cursor());
            page.terminal = PageTerminal::More(next);
            *self.offered.lock().expect("offered") = Some(next);
        }
        Ok(reply)
    }
}

#[test]
fn direct_member_session_refuses_changed_reply_sources_before_retaining_any_cursor() {
    let (owner, requests, center) = direct_member_fixture();
    let source = {
        let library = owner.lock().expect("owner");
        Basis {
            root: library.revision_root(),
            ..library.view().basis()
        }
    };
    let mut sources = [source; 5];
    sources[0].root = view_state_root(&[]);
    sources[1].object = object_version(b"changed returned object");
    sources[2].branch = backend_library::branch_key("changed returned branch");
    sources[3].log = backend_library::log_key("changed returned log");
    sources[4].schema = source.schema + 1;
    for changed in sources {
        assert_ne!(changed, source);
        let offered = Arc::new(Mutex::new(None));
        let mut client = Session::from_transport(
            "/private/test-owner.sock",
            ChangedMemberReplySource {
                inner: BoundedMembersTransport::new(Transport {
                    owner: owner.clone(),
                    requests: requests.clone(),
                    graph_ids: None,
                }),
                source: changed,
                offered: offered.clone(),
            },
        );
        let reply = client
            .document_symbol(center)
            .expect("actual admitted owner document");
        let CommandReply::Document(document) = reply.reply else {
            panic!("document")
        };
        assert!(
            matches!(
                client.members_page(&document, 1, None),
                Err(ClientError::StaleCursor)
            ),
            "changed returned source must refuse: {changed:?}"
        );
        let successor = offered
            .lock()
            .expect("offered")
            .expect("transport offered a cursor");
        assert!(
            client.continuations.is_empty(),
            "no rejected member cursor is retained"
        );
        assert!(!client.has_portable_query_continuation(successor));
        assert!(client.encode_query_continuation(successor).is_err());
    }
}

#[test]
fn portable_outline_windows_reopen_legacy_and_variable_spans_without_losing_exact_rows() {
    let owner = Arc::new(Mutex::new(owner_with_package(
        1,
        11,
        0,
        &["Thing"],
        Some("/p"),
    )));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let expected = owner
        .lock()
        .expect("owner")
        .view()
        .rows()
        .iter()
        .filter_map(|row| matches!(row.id, RowId::Symbol(_)).then_some(row.id))
        .collect::<std::collections::BTreeSet<_>>();
    let mut issuing = session(&owner, &requests);
    let first = issuing
        .outline_page("/p", 4, None)
        .expect("legacy fixed page");
    let mut ids = window_snapshot(&first)
        .root
        .rows()
        .iter()
        .map(|row| row.id)
        .collect::<std::collections::BTreeSet<_>>();
    let mut next =
        PageContinuation::from_cursor(window_snapshot(&first).next.expect("legacy next"));
    let mut signed = issuing
        .encode_query_continuation(next)
        .expect("portable fixed predecessor");
    for delivery in [1, 2, 4] {
        let mut fresh = session(&owner, &requests);
        let imported = fresh
            .decode_page_continuation(&signed)
            .expect("fresh proof admission");
        assert_eq!(imported, next);
        let reply = fresh
            .outline_page_window("/p", delivery_window(4, delivery), Some(imported))
            .expect("native successor");
        let page = window_snapshot(&reply);
        assert!(page.root.row_count() <= u64::from(delivery));
        for row in page.root.rows() {
            assert!(ids.insert(row.id), "one exact ID once");
        }
        if let Some(cursor) = page.next {
            assert_eq!(
                cursor.query_offset(),
                imported.cursor().query_offset() + page.root.row_count()
            );
            assert_eq!(
                cursor
                    .query_position()
                    .predecessor()
                    .expect("window span")
                    .delivery()
                    .get(),
                delivery
            );
            next = PageContinuation::from_cursor(cursor);
            signed = fresh
                .encode_query_continuation(next)
                .expect("portable exact order-bound span");
            let mut wrong = token_value(&signed);
            wrong["command"]["data"]["page"]["continuation"]["query_window"]["delivery"] =
                serde_json::json!(3);
            assert!(
                session(&owner, &requests)
                    .decode_page_continuation(&token(&wrong))
                    .is_err(),
                "tampered predecessor span refuses"
            );
            let mut changed = session(&owner, &requests);
            let imported = changed
                .decode_page_continuation(&signed)
                .expect("same exact proof");
            let before = requests.lock().expect("requests").len();
            assert!(
                changed
                    .outline_page_window("/p", delivery_window(3, 1), Some(imported))
                    .is_err()
            );
            assert_eq!(
                requests.lock().expect("requests").len(),
                before,
                "changed issued credit refuses before page submission"
            );
        } else {
            assert_eq!(delivery, 4);
        }
    }
    assert_eq!(ids, expected);
}
