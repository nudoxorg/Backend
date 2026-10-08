#![allow(clippy::expect_used)]

use super::*;
use backend_library::{
    AuthorityScopeClaim, Basis, Coverage, CoverageCapability, Cursor, Frontier, Library,
    ProducerObservationClaims, ProducerObservationVerifier, QueryPageRecipe, Row, RowId, ScopeRoot,
    UntrustedProducerObservation, ViewRoot, admit_complete_scope, admit_producer_observation,
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
    let rows = labels
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
        if matches!(&request.command, Command::GraphPage { .. }) {
            let bytes =
                backend_library::encode_command_body(&request).map_err(ClientError::Protocol)?;
            let admitted =
                backend_library::decode_command_body(&bytes).map_err(ClientError::Protocol)?;
            assert_eq!(
                admitted, request,
                "plain graph strict standalone command admission"
            );
        }
        let mut reply = if let Command::GraphQuery(query) = &request.command {
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
            CommandReply::Search(page) | CommandReply::Names(page) => Some(page),
            CommandReply::ProjectionPage(page) => Some(&page.snapshot),
            _ => None,
        };
        if let Some(page) = snapshot {
            let recipe = match &request.command {
                Command::Search(query) => QueryPageRecipe::search(library.revision_root(), query),
                Command::Name(query) => QueryPageRecipe::names(library.revision_root(), query),
                Command::GraphPage { symbol, .. } => {
                    QueryPageRecipe::graph(library.revision_root(), *symbol)
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
                    if graph_address_claim(&request.command, claim) {
                        proof = proof.with_claim_once(claim.clone());
                    }
                }
            }
            let root = &page.root;
            for claim in [
                WireClaim::KeyBytes {
                    schema: WireSchema::ViewRecipe,
                    id: encode_id(root.recipe().as_bytes()),
                    value: recipe.canonical_preimage().into(),
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
                if let RowId::Symbol(symbol) = row.id {
                    proof = proof.with_claim_once(WireClaim::KeyCommitment {
                        schema: WireSchema::Symbol,
                        id: encode_id(symbol.as_bytes()),
                    });
                }
            }
            reply = reply.with_certificate(proof);
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
fn portable_query_full_credit_can_export_a_compact_predecessor() {
    let owner = Arc::new(Mutex::new(owner(1, 401, 0)));
    let requests = Arc::new(Mutex::new(vec![]));
    let mut session = session(&owner, &requests);
    let reply = session
        .names_page("Thing", 200, None)
        .expect("full credit query");
    let next = page(&reply).next.expect("successor exists");
    let exported = session.encode_query_continuation(PageContinuation::from_cursor(next));
    assert!(exported.is_ok(), "compact export: {exported:?}");
}

#[test]
fn portable_query_compressible_long_proof_preserves_every_canonical_byte() {
    let text = "x".repeat(backend_library::MAX_COMMAND_TEXT);
    let owner = Arc::new(Mutex::new(owner_with_label(1, 4, 0, &text)));
    let requests = Arc::new(Mutex::new(vec![]));
    let mut session = session(&owner, &requests);
    let reply = session
        .names_page(&text, 3, None)
        .expect("admitted bounded long query");
    let next = page(&reply).next.expect("successor exists");
    let exported = session.encode_query_continuation(PageContinuation::from_cursor(next));
    let token = exported.expect("bounded compact proof");
    assert!(token.starts_with("pc3-"));
    let body = decode_portable_body(&token).expect("bounded expansion");
    backend_library::decode_command_body(&body).expect("strict canonical proof remains intact");
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
    std::io::Write::write_all(&mut encoder, &oversized).expect("compressed adversary");
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
    assert!(token.starts_with("pc3-"));
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
    let package = backend_library::package_key("graph-package");
    let source = symbol_key("graph::source");
    let mut rows = vec![
        Row::new(RowId::Package(package), basis, "graph-package"),
        Row::in_package(RowId::Symbol(source), basis, package, "graph::source"),
    ];
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
            "copied::semantic::source"
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
        assert!(token.starts_with("pc3-"));
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
