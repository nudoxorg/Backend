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
    let rows = (0..count)
        .map(|i| {
            Row::new(
                RowId::Symbol(symbol_key(&format!("pkg::Thing{i:03}"))),
                basis,
                label,
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
}
impl CommandTransport for Transport {
    fn request(&mut self, request: CommandDto) -> Result<ReplyDto, ClientError> {
        self.requests
            .lock()
            .expect("requests")
            .push(request.command.clone());
        let library = self.owner.lock().expect("owner");
        let mut reply = library.execute_dto(request.clone());
        let snapshot = match &reply.reply {
            CommandReply::Search(page) | CommandReply::Names(page) => Some(page),
            _ => None,
        };
        if let Some(page) = snapshot {
            let recipe = match &request.command {
                Command::Search(query) => QueryPageRecipe::search(library.revision_root(), query),
                Command::Name(query) => QueryPageRecipe::names(library.revision_root(), query),
                _ => unreachable!("query reply"),
            };
            let mut proof = library
                .execute_dto(CommandDto::new(1, Command::Revision))
                .certificate()
                .expect("revision certificate")
                .clone();
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
    serde_json::from_slice(&decode_hex(token.strip_prefix("pc2-").expect("version")).expect("hex"))
        .expect("envelope")
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
    token_bytes(&serde_json::to_vec(value).expect("wire"))
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
    let body = decode_hex(good.strip_prefix("pc2-").expect("version")).expect("hex");
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
fn portable_query_export_budget_refuses_instead_of_hiding_a_successor() {
    let text = "x".repeat(backend_library::MAX_COMMAND_TEXT);
    let owner = Arc::new(Mutex::new(owner_with_label(1, 4, 0, &text)));
    let requests = Arc::new(Mutex::new(vec![]));
    let mut session = session(&owner, &requests);
    let reply = session
        .names_page(&text, 3, None)
        .expect("admitted bounded long query");
    let next = page(&reply).next.expect("successor exists");
    let exported = session.encode_query_continuation(PageContinuation::from_cursor(next));
    assert!(
        matches!(
            exported,
            Err(ClientError::Transport(ReplicationError::MessageTooLarge))
        ),
        "oversized existing canonical proof must refuse: {exported:?}"
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
        RebindingTransport(Transport { owner, requests }),
    );
    let cursor = fresh
        .decode_page_continuation(&token)
        .expect("revision admits old scope");
    assert!(
        fresh.search_page("Thing", 3, Some(cursor)).is_err(),
        "same root/new scope terminal page must refuse"
    );
}
