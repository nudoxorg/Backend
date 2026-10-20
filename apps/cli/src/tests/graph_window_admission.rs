//! Native Library/certificate/Session laws for the CLI graph output-admission retry.
use super::*;
use crate::{CommandDto, CommandTransport, admit_reply};
use backend_library::{
    Basis, Command, CommandReply, Coverage, Cursor, DeclarationSelector, Frontier, Library,
    QueryLimit, QueryWindow, Row, RowId, ViewRoot, WireClaim, WireSchema, encode_id,
    object_version, package_key, symbol_key, view_key, view_state_root,
};
use std::sync::{Arc, Mutex};

const AT: &str = "pkg::Parent";
const ISSUED: u16 = 8;

fn fixture_projection_capability(basis: Basis) -> backend_library::CoverageCapability {
    use backend_library::{
        AuthorityScopeClaim, CoverageCapability, ProducerObservationClaims,
        ProducerObservationVerifier, ScopeRoot, UntrustedProducerObservation, admit_complete_scope,
        admit_producer_observation,
    };
    struct ProjectionVerifier;
    impl ProducerObservationVerifier for ProjectionVerifier {
        type Error = &'static str;
        fn verify(
            &self,
            observation: &UntrustedProducerObservation,
        ) -> Result<ProducerObservationClaims, Self::Error> {
            if observation.producer_identity() != [7; 32]
                || observation.context() != [8; 32]
                || observation.evidence() != [9; 32]
            {
                return Err("foreign test projection producer");
            }
            Ok(ProducerObservationClaims::new(
                observation.producer_identity(),
                observation.scope_root(),
                observation.context(),
                *blake3::hash(observation.evidence()).as_bytes(),
            ))
        }
    }
    let observation = admit_producer_observation(
        UntrustedProducerObservation::new(
            [7; 32],
            ScopeRoot::from_bytes(basis.object.to_bytes()),
            [8; 32],
            vec![9; 32],
        ),
        &ProjectionVerifier,
    )
    .expect("closed projection observation");
    CoverageCapability::from_authorized_with_evidence(
        admit_complete_scope(
            AuthorityScopeClaim::from_object_version(basis.object),
            observation,
        )
        .expect("projection source scope"),
        vec![9; 32],
    )
    .expect("projection capability")
}

struct GraphTransport(Arc<Mutex<Library>>, Arc<Mutex<Vec<CommandDto>>>);
impl CommandTransport for GraphTransport {
    fn request(&mut self, request: CommandDto) -> Result<ReplyDto, ClientError> {
        self.1
            .lock()
            .expect("observed native command")
            .push(request.clone());
        let owner = self.0.lock().expect("immutable graph owner");
        let mut reply = owner.execute_dto(request.clone());
        if let (
            Command::GraphPage {
                symbol,
                page: request_page,
            },
            CommandReply::ProjectionPage(page),
        ) = (&request.command, &reply.reply)
        {
            let recipe = backend_library::QueryPageRecipe::graph_window(
                owner.revision_root(),
                *symbol,
                request_page.window(),
            );
            let mut proof = owner
                .execute_dto(CommandDto::new(1, Command::Revision))
                .certificate()
                .expect("owner scope")
                .clone();
            // The selected graph operand and exact canonical predecessor
            // come from this fixture producer's admitted request and page.
            if let Some(request_proof) = request.certificate() {
                for claim in &request_proof.claims {
                    proof = proof.with_claim_once(claim.clone());
                }
            }
            let root = &page.snapshot.root;
            for claim in [
                WireClaim::KeyBytes {
                    schema: WireSchema::ViewRecipe,
                    id: encode_id(root.recipe().as_bytes()),
                    value: recipe.canonical_preimage().into(),
                },
                WireClaim::Version {
                    schema: WireSchema::ViewVersion,
                    id: encode_id(root.version().as_bytes()),
                    value: backend_library::view_version_preimage(
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
                        .expect("exact relation")
                        .into_boxed_slice(),
                },
            ] {
                proof = proof.with_claim_once(claim);
            }
            for row in root.rows() {
                if let RowId::Symbol(symbol) = row.id {
                    proof = proof.with_claim_once(WireClaim::KeyCommitment {
                        schema: WireSchema::Symbol,
                        id: encode_id(symbol.as_bytes()),
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
        let bytes = serde_json::to_vec(&reply).expect("producer wire");
        let reply = ReplyDto::decode_with_certificate(&bytes, owner.view().capability())
            .map_err(ClientError::Protocol)?;
        admit_reply(&request, reply)
    }
}

fn owner(version: u8) -> Library {
    let basis = Basis::new(
        view_state_root(&[("graph-source".to_owned(), version.to_string())]),
        object_version(&[version]),
    );
    let parent = symbol_key(AT);
    let package = package_key("pkg");
    let mut rows = vec![Row::in_package(RowId::Symbol(parent), basis, package, AT)];
    for number in 0..12 {
        let label = format!("pkg::Child{number:02}_{}", "界אב".repeat(256));
        let mut row = Row::in_package(RowId::Symbol(symbol_key(&label)), basis, package, label);
        row.parent = Some(parent);
        rows.push(row);
    }
    let root = ViewRoot::new_checked(
        view_key(b"graph-window-output"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
        rows,
        vec![Coverage::Complete],
        fixture_projection_capability(basis),
    )
    .expect("real closed projection owner");
    Library::from_view(root.clone(), Cursor::for_view_root(&root)).expect("immutable graph library")
}

fn client(owner: Arc<Mutex<Library>>, commands: Arc<Mutex<Vec<CommandDto>>>) -> Session {
    Session::from_transport(
        "/private/graph-window-owner.sock",
        GraphTransport(owner, commands),
    )
}

fn graph_request() -> Request {
    Request::GraphPage {
        selector: DeclarationSelector::Coordinate(AT.to_owned()),
        limit: ISSUED,
    }
}

fn wire(session: &Session, answer: &Answer, budget: usize) -> Result<Vec<u8>, Fault> {
    let cursor = answer
        .continuation()
        .map(|cursor| session.encode_query_continuation(cursor))
        .transpose()
        .expect("export only native admitted continuation");
    backend_present::encode_answer(
        answer,
        backend_present::Detail::Full,
        cursor.as_deref(),
        budget,
    )
    .map(|payload| payload.bytes.into_vec())
    .map_err(backend_present::oversized_fault)
}

fn continued_answer(owner: Arc<Mutex<Library>>, token: &str, delivery: u16) -> (Vec<u8>, Answer) {
    let mut session = client(owner, Arc::new(Mutex::new(Vec::new())));
    let input = session
        .decode_page_continuation(token)
        .expect("genuine input proof");
    let window = QueryWindow::new(
        QueryLimit::new(ISSUED).expect("issued"),
        QueryLimit::new(delivery).expect("delivery"),
    )
    .expect("original credit");
    let reply = session
        .graph_page_window(AT, window, Some(input))
        .expect("native calibration page");
    let answer = backend_present::catalog_page_answer(&graph_request(), reply)
        .expect("native graph projection");
    (
        wire(&session, &answer, usize::MAX).expect("complete native calibration encoding"),
        answer,
    )
}

struct Observed<'a> {
    inner: SessionEngine<'a>,
    windows: Vec<QueryWindow>,
    inputs: Vec<Option<PageContinuation>>,
    sources: Vec<(Basis, u64)>,
    successors: Vec<Option<PageContinuation>>,
    imports: Vec<PageContinuation>,
}
impl Engine for Observed<'_> {
    fn revision(&mut self) -> Result<ViewStateRoot, ClientError> {
        self.inner.revision()
    }
    fn health(&mut self) -> Result<HealthReport, ClientError> {
        self.inner.health()
    }
    fn probe(&mut self, probe: Probe<'_>) -> Result<ReplyDto, ClientError> {
        self.inner.probe(probe)
    }
    fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
        self.inner.surface(command)
    }
    fn prepare_query_continuation(&mut self, token: &str) -> Result<PageContinuation, ClientError> {
        let input = self.inner.prepare_query_continuation(token)?;
        self.imports.push(input);
        Ok(input)
    }
    fn declaration_graph_window(
        &mut self,
        selector: &DeclarationSelector,
        window: QueryWindow,
        input: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        self.windows.push(window);
        self.inputs.push(input);
        let reply = self
            .inner
            .declaration_graph_window(selector, window, input)?;
        let answer = backend_present::catalog_page_answer(&graph_request(), reply.clone())
            .expect("actual admitted source scope");
        let Answer::Records(records) = &answer else {
            panic!("graph records");
        };
        self.sources
            .push(records.page_basis().expect("native source basis"));
        self.successors.push(answer.continuation());
        Ok(reply)
    }
}

fn setup() -> (Arc<Mutex<Library>>, String, PageContinuation) {
    let owner = Arc::new(Mutex::new(owner(1)));
    let mut session = client(owner.clone(), Arc::new(Mutex::new(Vec::new())));
    let window = QueryWindow::new(
        QueryLimit::new(ISSUED).expect("eight"),
        QueryLimit::new(2).expect("two"),
    )
    .expect("seed window");
    let reply = session
        .graph_page_window(AT, window, None)
        .expect("genuine seed window");
    let answer =
        backend_present::catalog_page_answer(&graph_request(), reply).expect("seed projection");
    let input = answer.continuation().expect("seed advances after two rows");
    let token = session
        .encode_query_continuation(input)
        .expect("genuine seed portable proof");
    (owner, token, input)
}

#[test]
fn cli_graph_output_retry_keeps_credit_input_source_and_exports_only_accepted_frontier() {
    let (owner, token, input) = setup();
    let (small, expected) = continued_answer(owner.clone(), &token, 1);
    let (two, _) = continued_answer(owner.clone(), &token, 2);
    let (large, _) = continued_answer(owner.clone(), &token, ISSUED);
    let budget = small.len() + 256;
    assert!(
        large.len() > budget,
        "the complete actual encoding forces an oversize retry"
    );
    assert!(
        two.len() > budget,
        "actual two-row encoding also exceeds the cap"
    );
    let commands = Arc::new(Mutex::new(Vec::new()));
    let mut session = client(owner.clone(), commands.clone());
    let mut engine = Observed {
        inner: SessionEngine::new(&mut session),
        windows: vec![],
        inputs: vec![],
        sources: vec![],
        successors: vec![],
        imports: vec![],
    };
    let selector = DeclarationSelector::Coordinate(AT.to_owned());
    let mut oversized = 0;
    let mut accepted = None;
    let answer = graph_with_output_admission(
        &mut engine,
        &selector,
        ISSUED,
        Some(&token),
        || Ok(()),
        |engine, answer| match wire(engine.inner.session, answer, budget) {
            Ok(bytes) => {
                accepted = Some(bytes);
                Ok(())
            }
            Err(error) => {
                assert_eq!(error.cause().slug(), backend_present::CauseSlug::Oversized);
                oversized += 1;
                Err(error)
            }
        },
    )
    .expect("complete native graph page after actual encoding admits it");
    assert!(oversized > 0);
    assert!(engine.windows.len() > 1);
    assert!(
        engine
            .windows
            .iter()
            .all(|window| window.issued().get() == ISSUED)
    );
    assert!(
        engine
            .windows
            .windows(2)
            .all(|pair| pair[1].delivery().get() < pair[0].delivery().get())
    );
    assert!(
        engine
            .inputs
            .iter()
            .all(|observed| *observed == Some(input))
    );
    assert!(engine.imports.iter().all(|observed| *observed == input));
    assert_eq!(
        engine.imports.len(),
        engine.windows.len(),
        "reimport exact input at every retry"
    );
    assert!(
        engine
            .sources
            .iter()
            .all(|basis| *basis == engine.sources[0])
    );
    let accepted_cursor = answer
        .continuation()
        .expect("accepted page has a successor");
    assert_eq!(
        Some(accepted_cursor),
        *engine.successors.last().expect("final native candidate")
    );
    assert_ne!(
        Some(accepted_cursor),
        engine.successors[0],
        "discarded larger-page cursor must not escape"
    );
    assert_eq!(
        answer.continuation(),
        expected.continuation(),
        "calibration and accepted one-row frontier agree"
    );
    let accepted = accepted.expect("only final native page was admitted");
    assert!(accepted.len() <= budget);
    let payload: serde_json::Value =
        serde_json::from_slice(&accepted).expect("complete final JSON");
    assert_eq!(payload["records"].as_array().expect("records").len(), 1);
    let token = payload["nextCursor"]
        .as_str()
        .expect("actual accepted portable cursor");
    let mut reopened = client(owner, Arc::new(Mutex::new(Vec::new())));
    assert_eq!(
        reopened
            .decode_page_continuation(token)
            .expect("cold admitted final proof"),
        accepted_cursor
    );
    let observed = commands.lock().expect("native requests");
    let pages = observed
        .iter()
        .filter_map(|request| match &request.command {
            Command::GraphPage { page, .. } => Some(page),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(pages.len(), engine.windows.len());
    assert!(
        pages.iter().all(
            |page| page.window().issued().get() == ISSUED && page.continuation() == Some(input)
        )
    );
}

#[test]
fn cli_graph_output_retry_refuses_real_owner_source_change() {
    let (owner, token, _) = setup();
    let (small, _) = continued_answer(owner.clone(), &token, 1);
    let (large, _) = continued_answer(owner.clone(), &token, ISSUED);
    let budget = small.len() + 256;
    assert!(large.len() > budget);
    let mut session = client(owner.clone(), Arc::new(Mutex::new(Vec::new())));
    let mut engine = SessionEngine::new(&mut session);
    let selector = DeclarationSelector::Coordinate(AT.to_owned());
    let mut rejected = 0;
    let error = graph_with_output_admission(
        &mut engine,
        &selector,
        ISSUED,
        Some(&token),
        || Ok(()),
        |engine, answer| {
            let result = wire(engine.session, answer, budget);
            if result.is_err() {
                rejected += 1;
                *owner.lock().expect("change genuine owner source") =
                    super::graph_window_admission_tests::owner(2);
            }
            result.map(|_| ())
        },
    )
    .expect_err("new source cannot replace the admitted input frontier during retry");
    assert_eq!(
        rejected, 1,
        "changed source refuses before another output admission"
    );
    assert_eq!(
        error.cause().slug(),
        backend_present::CauseSlug::Moved,
        "real owner replacement must report source movement"
    );
}
