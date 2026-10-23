#![allow(clippy::expect_used)]

use crate::canonical::{
    object_version, symbol_key, view_key, view_state_root, view_version_preimage,
};
use crate::{
    AuthorityScopeClaim, Basis, Command, CommandDto, CommandReply, CompleteViewProjection,
    CoverageCapability, Cursor, CursorEvent, DTO_VERSION, Document, DocumentQuery, EventDto,
    Fragment, Freshness, Frontier, GraphNeighborhoodQuery, MAX_ROW_IDENTITY_PREIMAGE_BYTES,
    NameQuery, Outline, OutlineNode, OutlineQuery, Query, QueryLimit, ReplyDto,
    RequestAdmissionError, Row, RowId, RowIdentityPreimage, ScopeRoot, SemanticObject,
    SnapshotPageDto, SourceAvailability, SourceExcerpt, SourceExcerptExtent, SourceLocation,
    SubscriptionDto, ViewDelta, ViewDto, ViewError, ViewPageCursor, ViewRoot, ViewSnapshot,
    ViewStateRoot, WireCertificate, WireClaim, WireSchema, admit_reply,
    admit_reply_with_capability, admit_request, encode_id,
};

pub(super) fn capability(object: SemanticObject) -> CoverageCapability {
    let declared = AuthorityScopeClaim::from_object_version(object);
    let scope = ScopeRoot::from_bytes(object.to_bytes());
    let observation = crate::admit_producer_observation(
        crate::UntrustedProducerObservation::new(
            *scope.as_bytes(),
            scope,
            *scope.as_bytes(),
            scope.as_bytes().to_vec(),
        ),
        &TestCoverageVerifier,
    )
    .expect("producer observation");
    CoverageCapability::from_authorized_with_evidence(
        crate::admit_complete_scope(declared, observation).expect("producer coverage"),
        scope.as_bytes().to_vec(),
    )
    .expect("coverage evidence")
}

fn paged_query_fixture(sequence: u64) -> (crate::Library, Vec<RowId>) {
    let basis = Basis::new(view_state_root(&[]), object_version(b"paged-client-source"));
    let ids = (0..3)
        .map(|i| RowId::Symbol(symbol_key(&format!("pkg::Thing{i}"))))
        .collect::<Vec<_>>();
    let rows = ids
        .iter()
        .enumerate()
        .map(|(i, id)| Row::new(*id, basis, format!("Thing{i}")))
        .collect::<Vec<_>>();
    let root = ViewRoot::new_checked(
        view_key(b"paged-client"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
        rows,
        vec![crate::Coverage::Complete],
        capability(basis.object),
    )
    .expect("checked owner view");
    let library =
        crate::Library::from_view(root.clone(), Cursor::for_view_root_at(&root, sequence))
            .expect("owner library");
    (library, ids)
}

#[test]
fn query_continuation_admits_the_successor_page_through_the_client_boundary() {
    let (library, ids) = paged_query_fixture(0);
    let limit = QueryLimit::new(1).expect("one row credit");
    let search = Query::new("Thing", library.revision_root(), limit);
    let first = library
        .search_from_ranked_ids(&search, &ids)
        .expect("first search page");
    assert_eq!(
        admit_reply(
            &CommandDto::new(1, Command::Search(search.clone())),
            &ReplyDto::new(1, CommandReply::Search(first.clone()))
        ),
        Ok(())
    );
    let search = search.with_cursor(first.next.expect("preceding page cursor"));
    let second = library
        .search_from_ranked_ids(&search, &ids)
        .expect("second search page");
    assert_ne!(first.root.root(), second.root.root());
    assert_eq!(
        admit_reply(
            &CommandDto::new(2, Command::Search(search)),
            &ReplyDto::new(2, CommandReply::Search(second))
        ),
        Ok(())
    );

    let names = NameQuery::new("Thing", library.revision_root(), limit);
    let first = library.names(&names).expect("first names page");
    let names = names.with_cursor(first.next.expect("names predecessor"));
    let second = library.names(&names).expect("second names page");
    assert_eq!(
        admit_reply(
            &CommandDto::new(3, Command::Name(names)),
            &ReplyDto::new(3, CommandReply::Names(second))
        ),
        Ok(())
    );
}

#[test]
fn query_continuation_preserves_intent_only_stream_progress_and_terminal_pages() {
    let (library, ids) = paged_query_fixture(5);
    assert_eq!(library.view().frontier().sequence, 0);
    assert_eq!(library.cursor().sequence(), 5);
    let mut query = Query::new(
        "Thing",
        library.revision_root(),
        QueryLimit::new(1).expect("limit"),
    );
    for page_index in 0..3 {
        let snapshot = library
            .search_from_ranked_ids(&query, &ids)
            .expect("ranked page");
        let request = CommandDto::new(page_index, Command::Search(query.clone()));
        assert_eq!(
            admit_reply(
                &request,
                &ReplyDto::new(page_index, CommandReply::Search(snapshot.clone()))
            ),
            Ok(())
        );
        if let Some(next) = snapshot.next {
            assert_eq!(next.sequence(), 5);
            query = query.with_cursor(next);
        } else {
            assert_eq!(page_index, 2);
        }
    }
    let names = NameQuery::new("NoMatches", library.revision_root(), QueryLimit::default());
    let empty = library.names(&names).expect("empty first page");
    assert_eq!(empty.root.row_count(), 0);
    assert_eq!(
        admit_reply(
            &CommandDto::new(10, Command::Name(names)),
            &ReplyDto::new(10, CommandReply::Names(empty))
        ),
        Ok(())
    );
}

#[test]
fn query_continuation_rejects_contract_substitution_and_forged_successors() {
    let (library, ids) = paged_query_fixture(0);
    let limit = QueryLimit::new(1).expect("limit");
    let query = Query::new("Thing", library.revision_root(), limit);
    let first = library
        .search_from_ranked_ids(&query, &ids)
        .expect("first page");
    let predecessor = first.next.expect("continuation");
    let continued = query.clone().with_cursor(predecessor);
    let second = library
        .search_from_ranked_ids(&continued, &ids)
        .expect("successor");
    let reply = ReplyDto::new(1, CommandReply::Search(second.clone()));
    let empty_manifest = crate::ReadManifest::new(vec![]).expect("empty manifest is explicit");
    for substituted in [
        Query::new("thing", library.revision_root(), limit).with_cursor(predecessor),
        Query::new(
            "Thing",
            library.revision_root(),
            QueryLimit::new(2).expect("different credit"),
        )
        .with_cursor(predecessor),
        query
            .clone()
            .with_read_manifest(empty_manifest)
            .with_cursor(predecessor),
    ] {
        assert!(
            library.search_from_ranked_ids(&substituted, &ids).is_err(),
            "producer proves predecessor under its exact contract"
        );
        assert!(admit_reply(&CommandDto::new(1, Command::Search(substituted)), &reply).is_err());
    }
    let names = NameQuery::new("Thing", library.revision_root(), limit).with_cursor(predecessor);
    assert!(
        library.names(&names).is_err(),
        "search cursor cannot become a names cursor"
    );
    assert!(
        admit_reply(
            &CommandDto::new(1, Command::Name(names)),
            &ReplyDto::new(1, CommandReply::Names(second.clone()))
        )
        .is_err()
    );

    let mut forged = second.clone();
    forged.next = Some(forged.next.expect("next").with_query_offset(u64::MAX));
    assert!(
        admit_reply(
            &CommandDto::new(1, Command::Search(continued.clone())),
            &ReplyDto::new(1, CommandReply::Search(forged))
        )
        .is_err()
    );
    let mut replay = first.clone();
    assert!(
        admit_reply(
            &CommandDto::new(1, Command::Search(continued.clone())),
            &ReplyDto::new(1, CommandReply::Search(replay.clone()))
        )
        .is_err(),
        "next offset cannot replay a preceding page"
    );
    replay.root = library.view().clone();
    replay.next = None;
    assert!(
        admit_reply(
            &CommandDto::new(1, Command::Search(continued)),
            &ReplyDto::new(1, CommandReply::Search(replay))
        )
        .is_err(),
        "owner rows are not a query projection"
    );

    let bad_predecessor = predecessor.with_query_offset(u64::MAX);
    assert!(
        library
            .search_from_ranked_ids(&query.clone().with_cursor(bad_predecessor), &ids)
            .is_err()
    );
    let zero_page = predecessor.with_query_offset(0);
    assert!(
        library
            .search_from_ranked_ids(&query.clone().with_cursor(zero_page), &ids)
            .is_err(),
        "page root cannot masquerade as owner start"
    );
    let owner = library.cursor();
    let start = Cursor::for_view(
        first.root.recipe(),
        library.view().version(),
        Frontier::new(
            owner.branch(),
            owner.log(),
            owner.schema(),
            owner.root(),
            owner.sequence(),
        ),
    );
    let query = query.with_cursor(start);
    let page = library
        .search_from_ranked_ids(&query, &ids)
        .expect("exact owner-bound zero-offset start");
    assert_eq!(
        admit_reply(
            &CommandDto::new(1, Command::Search(query)),
            &ReplyDto::new(1, CommandReply::Search(page))
        ),
        Ok(())
    );
}

#[test]
fn semantic_link_target_uses_the_admitted_producer_commitment() {
    let source_root = view_state_root(&[]);
    let basis = Basis::new(source_root, object_version(b"semantic-link-source"));
    let target = symbol_key("semantic::opaque::Target");
    let target_id = encode_id(target.as_bytes());
    let certificate = WireCertificate::new()
        // The semantic symbol has no textual preimage.  A hostile or
        // structural projection may still attach a stale label, so the
        // receiver must use the producer commitment after digest mismatch.
        .with_claim(WireClaim::Key {
            schema: WireSchema::Symbol,
            id: target_id.clone(),
            value: "stale structural label".to_owned(),
        })
        .with_claim(WireClaim::KeyCommitment {
            schema: WireSchema::Symbol,
            id: target_id.clone(),
        });
    let wire = super::reply_content::FragmentWire::Link(super::reply_content::LinkWire {
        label: "Target".to_owned(),
        target: target_id,
    });
    assert!(
        super::reply_content::fragment_from_wire_with_capability(wire.clone(), &certificate, None)
            .is_err(),
        "opaque semantic link must not decode without producer coverage"
    );
    let decoded = super::reply_content::fragment_from_wire_with_capability(
        wire,
        &certificate,
        Some(&capability(basis.object)),
    )
    .expect("producer commitment should admit an opaque semantic link target");
    assert!(matches!(
        decoded,
        Fragment::Link {
            label,
            target: decoded_target,
        } if label == "Target" && decoded_target == target
    ));
}

struct TestCoverageVerifier;

impl crate::ProducerObservationVerifier for TestCoverageVerifier {
    type Error = &'static str;

    fn verify(
        &self,
        observation: &crate::UntrustedProducerObservation,
    ) -> Result<crate::ProducerObservationClaims, Self::Error> {
        if observation.producer_identity() == *observation.scope_root().as_bytes()
            && observation.context() == *observation.scope_root().as_bytes()
            && observation.evidence() == observation.scope_root().as_bytes()
        {
            Ok(crate::ProducerObservationClaims::new(
                observation.producer_identity(),
                observation.scope_root(),
                observation.context(),
                *blake3::hash(observation.evidence()).as_bytes(),
            ))
        } else {
            Err("invalid test producer observation")
        }
    }
}

pub(super) fn certificate(root: &ViewRoot) -> WireCertificate {
    let cursor = Cursor::for_view_root(root);
    let mut certificate = WireCertificate::new().with_claim(WireClaim::KeyBytes {
        schema: WireSchema::ViewRecipe,
        id: encode_id(root.recipe().as_bytes()),
        value: b"view".to_vec().into_boxed_slice(),
    });
    certificate = certificate.with_claim(WireClaim::Version {
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
    });
    certificate = certificate.with_claim(WireClaim::Root {
        schema: WireSchema::ViewRelation,
        id: encode_id(root.root().as_bytes()),
        canonical: root
            .canonical_relation_bytes()
            .expect("root canonical bytes")
            .into_boxed_slice(),
    });
    if root.root() != root.basis().root {
        certificate = certificate.with_claim(WireClaim::Root {
            schema: WireSchema::ViewRelation,
            id: encode_id(root.basis().root.as_bytes()),
            canonical: backend_version::canonical_empty::<crate::ViewRelation>()
                .as_bytes()
                .to_vec()
                .into_boxed_slice(),
        });
    }
    certificate = certificate.with_claim(WireClaim::Version {
        schema: WireSchema::Object,
        id: encode_id(root.basis().object.as_bytes()),
        value: b"source".to_vec().into_boxed_slice(),
    });
    certificate = certificate.with_claim(WireClaim::Coverage {
        scope: encode_id(root.basis().object.as_bytes()),
        observed: encode_id(root.basis().object.as_bytes()),
        producer: encode_id(&capability(root.basis().object).producer_identity()),
        context: encode_id(&capability(root.basis().object).context()),
        evidence: capability(root.basis().object)
            .evidence()
            .to_vec()
            .into_boxed_slice(),
    });
    certificate = certificate.with_claim(WireClaim::Key {
        schema: WireSchema::Branch,
        id: encode_id(root.basis().branch.as_bytes()),
        value: "main".to_owned(),
    });
    certificate
        .with_claim(WireClaim::Key {
            schema: WireSchema::Log,
            id: encode_id(root.basis().log.as_bytes()),
            value: "library".to_owned(),
        })
        .with_claim(WireClaim::Cursor {
            recipe: encode_id(cursor.recipe().as_bytes()),
            version: encode_id(cursor.version().as_bytes()),
            branch: encode_id(cursor.branch().as_bytes()),
            log: encode_id(cursor.log().as_bytes()),
            schema: cursor.schema(),
            root: encode_id(cursor.root().as_bytes()),
            sequence: cursor.sequence(),
        })
}

#[test]
fn event_dto_round_trips_a_checked_view_transition() {
    let source_root = view_state_root(&[]);
    let basis = Basis::new(source_root, object_version(b"source"));
    let view_id = view_key(b"view");
    let base = ViewRoot::empty_checked(
        view_id,
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, source_root, 0),
        capability(basis.object),
    )
    .expect("checked view root");
    let row = Row::new(RowId::Symbol(symbol_key("pkg::Thing")), basis, "Thing");
    let prepared = base
        .prepare(ViewDelta::Upsert { row }, capability(basis.object))
        .expect("prepare");
    let (target, delta) = base.commit(prepared).expect("commit");
    let dto = EventDto::new(
        Cursor::for_view(
            target.recipe,
            target.version,
            Frontier::new(
                target.frontier.branch,
                target.frontier.log,
                target.frontier.schema,
                target.root,
                target.frontier.sequence,
            ),
        ),
        CursorEvent::View {
            delta: Box::new(delta),
        },
    );
    let encoded = serde_json::to_string(&dto).expect("encode");
    assert!(serde_json::from_str::<EventDto>(&encoded).is_err());
    let decoded = EventDto::decode_against(encoded.as_bytes(), &dto).expect("decode");
    assert_eq!(decoded, dto);
    let mut forged: serde_json::Value = serde_json::from_str(&encoded).expect("value");
    forged["cursor"]["root"] = serde_json::json!(encode_id(source_root.as_bytes()));
    assert!(
        EventDto::decode_against(
            serde_json::to_string(&forged)
                .expect("encode forged")
                .as_bytes(),
            &dto,
        )
        .is_err()
    );

    let mut forged_scope: serde_json::Value = serde_json::from_str(&encoded).expect("value");
    forged_scope["event"]["data"]["coverage_scope"] =
        serde_json::json!(encode_id(object_version(b"different-source").as_bytes()));
    assert!(
        EventDto::decode_against(
            serde_json::to_string(&forged_scope)
                .expect("encode forged scope")
                .as_bytes(),
            &dto,
        )
        .is_err()
    );
}

#[test]
fn declaration_facts_cross_the_wire_with_their_text() {
    let source_root = view_state_root(&[]);
    let basis = Basis::new(source_root, object_version(b"source"));
    let view_id = view_key(b"view");
    let base = ViewRoot::empty_checked(
        view_id,
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, source_root, 0),
        capability(basis.object),
    )
    .expect("checked view root");
    let facts = crate::DeclarationFacts {
        deprecation: crate::Fact::Present(crate::Deprecation::new(
            Some("1.2.0"),
            Some("use `fresh`"),
        )),
        obligation: crate::Fact::Present(crate::Obligation::Required),
    };
    let row =
        Row::new(RowId::Symbol(symbol_key("pkg::stale")), basis, "stale").with_facts(facts.clone());
    let prepared = base
        .prepare(ViewDelta::Upsert { row }, capability(basis.object))
        .expect("prepare");
    let (target, delta) = base.commit(prepared).expect("commit");
    let dto = EventDto::new(
        Cursor::for_view(
            target.recipe,
            target.version,
            Frontier::new(
                target.frontier.branch,
                target.frontier.log,
                target.frontier.schema,
                target.root,
                target.frontier.sequence,
            ),
        ),
        CursorEvent::View {
            delta: Box::new(delta),
        },
    );
    let encoded = serde_json::to_string(&dto).expect("encode");
    assert!(encoded.contains(r#""since":"1.2.0""#), "{encoded}");
    assert!(encoded.contains(r#""note":"use `fresh`""#), "{encoded}");
    assert!(
        encoded.contains(r#""present","data":"required""#),
        "{encoded}"
    );
    let decoded = EventDto::decode_against(encoded.as_bytes(), &dto).expect("decode");
    assert_eq!(decoded, dto);
    let CursorEvent::View { delta } = &decoded.event else {
        panic!("a view event");
    };
    let ViewDelta::Upsert { row } = delta.delta() else {
        panic!("an upsert");
    };
    let notice = row
        .facts
        .deprecation
        .present()
        .expect("deprecation crossed");
    assert_eq!(notice.since(), Some("1.2.0"));
    assert_eq!(notice.note(), Some("use `fresh`"));
    assert_eq!(
        row.facts.obligation,
        crate::Fact::Present(crate::Obligation::Required)
    );

    // A document carries the same facts in both directions.
    let wire = super::reply_content::facts_to_wire(&facts);
    assert_eq!(
        super::reply_content::facts_from_wire(wire).expect("facts"),
        facts
    );
    // A producer that observed nothing adds nothing to the wire.
    assert!(super::reply_content::facts_to_wire(&crate::DeclarationFacts::UNOBSERVED).is_none());
}

#[test]
fn compact_view_event_is_delta_sized_and_replays_against_the_retained_root() {
    let source_root = view_state_root(&[]);
    let basis = Basis::new(source_root, object_version(b"source"));
    let base = ViewRoot::empty_checked(
        view_key(b"view"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, source_root, 0),
        capability(basis.object),
    )
    .expect("base");
    let row = Row::new(RowId::Symbol(symbol_key("pkg::Thing")), basis, "Thing");
    let prepared = base
        .prepare(ViewDelta::Upsert { row }, capability(basis.object))
        .expect("prepare");
    let (target, delta) = base.clone().commit(prepared).expect("commit");
    let certificate = WireCertificate::new()
        .with_claim(WireClaim::Delta {
            schema: WireSchema::ViewRelation,
            id: encode_id(delta.id().as_bytes()),
            base: encode_id(delta.base_root().as_bytes()),
            target: encode_id(delta.target_root().as_bytes()),
            changes: delta.canonical_changes().to_vec().into_boxed_slice(),
        })
        .with_claim(WireClaim::Key {
            schema: WireSchema::Symbol,
            id: encode_id(symbol_key("pkg::Thing").as_bytes()),
            value: "pkg::Thing".to_owned(),
        });
    let cursor = Cursor::for_view_root(&target);
    let encoded = crate::encode_compact_view_event(cursor, &delta, certificate)
        .expect("encode compact event");
    assert!(encoded.len() < 16 * 1024);
    let (decoded_cursor, decoded) =
        crate::decode_compact_view_event(&encoded, Cursor::for_view_root(&base), &base)
            .expect("decode compact event");
    assert_eq!(decoded_cursor, cursor);
    assert_eq!(decoded.target_root(), target.root());
    assert_eq!(decoded.target_view(), &target);
}

#[test]
fn occurrence_disambiguated_row_identity_is_admitted_from_its_explicit_preimage() {
    let source_root = view_state_root(&[]);
    let basis = Basis::new(source_root, object_version(b"source"));
    let preimage = "pkg::src/lib.rs:1::run\0method\0fn run()\01";
    let symbol = symbol_key(preimage);
    let row = Row::new(RowId::Symbol(symbol), basis, "pkg::src/lib.rs:1::run")
        .with_identity_preimage(RowIdentityPreimage::try_new(preimage).expect("bounded preimage"));
    let root = ViewRoot::new_checked(
        view_key(b"view"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, source_root, 0),
        vec![row],
        vec![crate::Coverage::Complete],
        capability(basis.object),
    )
    .expect("checked occurrence root");
    let admitted_certificate = certificate(&root).with_claim(WireClaim::RowIdentity {
        schema: WireSchema::Symbol,
        id: encode_id(symbol.as_bytes()),
        preimage: preimage.to_owned(),
    });
    let view = ViewDto::new(
        9,
        ViewSnapshot {
            root: root.clone(),
            freshness: Freshness::Current,
            next: None,
            graph_relations: None,
            rich_graph: None,
        },
    )
    .with_certificate(admitted_certificate);
    let encoded = serde_json::to_vec(&view).expect("encode occurrence view");
    ViewDto::decode_with_certificate(&encoded, Some(capability(basis.object)))
        .expect("explicit row identity should survive transport");

    let forged = certificate(&root).with_claim(WireClaim::RowIdentity {
        schema: WireSchema::Symbol,
        id: encode_id(symbol.as_bytes()),
        preimage: "pkg::src/lib.rs:1::run\\0function\\0fn run()\\01".to_owned(),
    });
    let forged_view = ViewDto::new(
        9,
        ViewSnapshot {
            root: root.clone(),
            freshness: Freshness::Current,
            next: None,
            graph_relations: None,
            rich_graph: None,
        },
    )
    .with_certificate(forged);
    let forged_encoded = serde_json::to_vec(&forged_view).expect("encode forged view");
    assert!(
        ViewDto::decode_with_certificate(&forged_encoded, Some(capability(basis.object))).is_err()
    );

    let mixed = certificate(&root)
        .with_claim(WireClaim::RowIdentity {
            schema: WireSchema::Symbol,
            id: encode_id(symbol.as_bytes()),
            preimage: preimage.to_owned(),
        })
        .with_claim(WireClaim::Key {
            schema: WireSchema::Symbol,
            id: encode_id(symbol.as_bytes()),
            value: preimage.to_owned(),
        });
    let mixed_view = ViewDto::new(
        9,
        ViewSnapshot {
            root: root.clone(),
            freshness: Freshness::Current,
            next: None,
            graph_relations: None,
            rich_graph: None,
        },
    )
    .with_certificate(mixed);
    let mixed_encoded = serde_json::to_vec(&mixed_view).expect("encode mixed view");
    assert!(
        ViewDto::decode_with_certificate(&mixed_encoded, Some(capability(basis.object))).is_err()
    );

    let oversized = certificate(&root).with_claim(WireClaim::RowIdentity {
        schema: WireSchema::Symbol,
        id: encode_id(symbol.as_bytes()),
        preimage: "x".repeat(MAX_ROW_IDENTITY_PREIMAGE_BYTES + 1),
    });
    assert!(
        oversized
            .row_identity_preimage(WireSchema::Symbol, &encode_id(symbol.as_bytes()))
            .is_err()
    );
}

#[test]
fn row_identity_witness_is_bounded_before_ownership_and_root_rechecks_the_digest() {
    let source_root = view_state_root(&[]);
    let basis = Basis::new(source_root, object_version(b"source"));
    let right = "pkg::src/lib.rs:1::run\0method\0fn run()\01";
    let wrong = symbol_key("pkg::src/lib.rs:1::run\0function\0fn run()\01");
    let witness = RowIdentityPreimage::try_new(right).expect("bounded witness");
    let row = Row::new(RowId::Symbol(wrong), basis, "pkg::src/lib.rs:1::run")
        .with_identity_preimage(witness);
    let error = ViewRoot::new_checked(
        view_key(b"view"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, source_root, 0),
        vec![row],
        vec![crate::Coverage::Complete],
        capability(basis.object),
    )
    .expect_err("a wrong witness digest must remain a typed identity failure");
    assert_eq!(error, ViewError::InvalidIdentity);
    assert!(
        RowIdentityPreimage::try_new(&"x".repeat(MAX_ROW_IDENTITY_PREIMAGE_BYTES + 1)).is_err()
    );
}

#[test]
fn compact_subscription_one_row_update_stays_bounded_on_a_large_view() {
    let source_root = view_state_root(&[]);
    let basis = Basis::new(source_root, object_version(b"source"));
    let mut base = ViewRoot::empty_checked(
        view_key(b"view"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, source_root, 0),
        capability(basis.object),
    )
    .expect("base");
    for index in 0..512 {
        let row = Row::new(
            RowId::Symbol(symbol_key(&format!("pkg::Large{index:04}"))),
            basis,
            "large",
        );
        let prepared = base
            .prepare(ViewDelta::Upsert { row }, capability(basis.object))
            .expect("large row prepare");
        (base, _) = base.commit(prepared).expect("large row commit");
    }
    let row = Row::new(RowId::Symbol(symbol_key("pkg::Tail")), basis, "tail");
    let prepared = base
        .prepare(ViewDelta::Upsert { row }, capability(basis.object))
        .expect("tail prepare");
    let (target, delta) = base.clone().commit(prepared).expect("tail commit");
    let certificate = WireCertificate::new()
        .with_claim(WireClaim::Delta {
            schema: WireSchema::ViewRelation,
            id: encode_id(delta.id().as_bytes()),
            base: encode_id(delta.base_root().as_bytes()),
            target: encode_id(delta.target_root().as_bytes()),
            changes: delta.canonical_changes().to_vec().into_boxed_slice(),
        })
        .with_claim(WireClaim::Key {
            schema: WireSchema::Symbol,
            id: encode_id(symbol_key("pkg::Tail").as_bytes()),
            value: "pkg::Tail".to_owned(),
        });
    let previous = Cursor::for_view_root(&base);
    let cursor = Cursor::for_view_root(&target);
    let event = EventDto::new(
        cursor,
        CursorEvent::View {
            delta: Box::new(delta),
        },
    )
    .with_certificate(certificate);
    let compact =
        crate::encode_compact_subscription(previous, cursor, std::slice::from_ref(&event))
            .expect("compact subscription");
    let full = serde_json::to_vec(
        &SubscriptionDto::try_events(previous, cursor, vec![event]).expect("full subscription"),
    )
    .expect("full encoding");
    assert!(compact.len() < 16 * 1024);
    assert!(full.len() > compact.len().saturating_mul(4));
    let read = SubscriptionDto::decode_against_root(&compact, previous, &base, None)
        .expect("compact subscription decode");
    assert!(
        matches!(read, crate::CursorRead::Events { cursor: observed, events } if observed == cursor && events.len() == 1)
    );
}

#[test]
fn command_dto_rejects_unknown_fields() {
    let json = r#"{
            "version": 1,
            "request_id": 7,
            "command": {"kind":"health","data":{},"extra":true}
        }"#;
    assert!(serde_json::from_str::<CommandDto>(json).is_err());
}

#[test]
fn expected_decode_rejects_forged_hash_and_context_claims() {
    let expected = CommandDto::new(
        4,
        Command::Add {
            cargo_target: None,
            package: crate::package_key("pkg"),
            execution_intent: crate::CompileExecutionIntent::Interactive,
        },
    );
    let mut forged = serde_json::to_value(&expected).expect("encode expected");
    forged["command"]["data"]["package"] =
        serde_json::json!(encode_id(symbol_key("pkg::Thing").as_bytes()));
    let encoded = serde_json::to_vec(&forged).expect("encode forged");
    assert!(CommandDto::decode_against(&encoded, &expected).is_err());

    let mut malformed = serde_json::to_value(&expected).expect("encode expected");
    malformed["command"]["data"]["package"] = serde_json::json!("not-a-digest");
    let encoded = serde_json::to_vec(&malformed).expect("encode malformed");
    assert!(CommandDto::decode_against(&encoded, &expected).is_err());
}

#[test]
fn add_execution_intent_has_one_strict_canonical_wire_spelling() {
    let command = CommandDto::new(
        9,
        Command::Add {
            cargo_target: None,
            package: crate::package_key("pkg"),
            execution_intent: crate::CompileExecutionIntent::Background,
        },
    );
    let mut value = serde_json::to_value(&command).expect("encode Add");
    assert_eq!(value["version"], serde_json::json!(DTO_VERSION));
    assert_eq!(value["command"]["data"]["execution_intent"], "background");
    let encoded = serde_json::to_vec(&command).expect("encode Add");
    assert_eq!(
        CommandDto::decode_against(&encoded, &command).expect("strict Add decode"),
        command
    );

    assert!(
        value["command"]["data"]
            .as_object_mut()
            .expect("Add data")
            .remove("execution_intent")
            .is_some()
    );
    let encoded = serde_json::to_vec(&value).expect("encode missing intent");
    assert!(CommandDto::decode_against(&encoded, &command).is_err());

    value["command"]["data"]["execution_intent"] = serde_json::json!("remote");
    let encoded = serde_json::to_vec(&value).expect("encode invalid intent");
    assert!(CommandDto::decode_against(&encoded, &command).is_err());
}

#[test]
fn command_dto_round_trips_every_command_variant() {
    let basis = view_state_root(&[]);
    let cursor = Cursor::new().with_query_offset(4);
    let package = crate::package_key("pkg");
    let symbol = symbol_key("pkg::Thing");
    let commands = [
        Command::Packages,
        Command::PackagePage(
            crate::PageRequest::new(basis, QueryLimit::default())
                .with_continuation(crate::PageContinuation::from_cursor(cursor)),
        ),
        Command::Add {
            cargo_target: None,
            package,
            execution_intent: crate::CompileExecutionIntent::Interactive,
        },
        Command::Remove { package },
        Command::Document(DocumentQuery {
            symbol: crate::SymbolAddress::canonical(symbol),
            basis: basis.into(),
            source: None,
        }),
        Command::Show { symbol },
        Command::Outline(OutlineQuery {
            package,
            basis: basis.into(),
            source: None,
        }),
        Command::OutlinePage {
            package,
            page: crate::PageRequest::new(basis, QueryLimit::default()),
        },
        Command::Name(NameQuery::new("Thing", basis, QueryLimit::default())),
        Command::Resolve {
            text: "Thing".to_owned(),
        },
        Command::Search(Query::new("Thing", basis, QueryLimit::default()).with_cursor(cursor)),
        Command::Graph(GraphNeighborhoodQuery::new(symbol, basis)),
        Command::GraphPage {
            symbol: crate::SymbolAddress::canonical(symbol),
            page: crate::PageRequest::new(basis, QueryLimit::default()),
        },
        Command::Health,
    ];
    for command in commands {
        let dto = CommandDto::new(11, command);
        let encoded = serde_json::to_vec(&dto).expect("encode command");
        if !matches!(
            &dto.command,
            Command::Packages | Command::Resolve { .. } | Command::Health
        ) {
            assert!(serde_json::from_slice::<CommandDto>(&encoded).is_err());
        }
        let decoded = CommandDto::decode_against(&encoded, &dto).expect("decode command");
        assert_eq!(decoded, dto);
    }
}

#[test]
fn selected_symbol_request_is_a_commitment_bound_locator() {
    let basis = view_state_root(&[]);
    let symbol = symbol_key("semantic identity whose display label may change");
    let command = CommandDto::new(
        37,
        Command::Graph(GraphNeighborhoodQuery::selected(symbol, basis)),
    );
    let basis_claim = WireClaim::RootCommitment {
        schema: WireSchema::ViewRelation,
        id: encode_id(basis.as_bytes()),
    };
    let without_symbol = command
        .clone()
        .with_certificate(WireCertificate::new().with_claim(basis_claim.clone()));
    let bytes = serde_json::to_vec(&without_symbol).expect("encode selected locator");
    assert!(
        serde_json::from_slice::<CommandDto>(&bytes).is_err(),
        "a digest-only selected locator must not cross the wire"
    );

    let admitted =
        command.with_certificate(WireCertificate::new().with_claim(basis_claim).with_claim(
            WireClaim::KeyCommitment {
                schema: WireSchema::Symbol,
                id: encode_id(symbol.as_bytes()),
            },
        ));
    let bytes = crate::encode_command_body(&admitted).expect("encode committed selector");
    let decoded = crate::decode_command_body(&bytes).expect("decode committed selector");
    assert_eq!(decoded, admitted);
    let Command::Graph(query) = decoded.command else {
        panic!("selected locator changed command shape");
    };
    assert!(query.symbol().is_selected());
    assert_eq!(query.symbol().claimed_bytes(), symbol.to_bytes());
}

#[test]
fn query_read_manifests_survive_the_strict_wire_round_trip() {
    let basis = view_state_root(&[]);
    let scope = ScopeRoot::from_bytes([23; backend_version::ID_BYTES]);
    let manifest = crate::ReadManifest::new(vec![
        crate::Read::range(backend_semantic::FacetKind::Name, scope, 17),
        crate::Read::negative_range(backend_semantic::FacetKind::Documentation, scope, 29),
    ])
    .expect("valid read manifest");
    let commands = [
        Command::Name(
            NameQuery::new("Thing", basis, QueryLimit::default())
                .with_read_manifest(manifest.clone()),
        ),
        Command::Search(
            Query::new("Thing", basis, QueryLimit::default()).with_read_manifest(manifest),
        ),
    ];

    for command in commands {
        let expected = CommandDto::new(41, command).with_certificate(
            WireCertificate::new().with_claim(WireClaim::RootCommitment {
                schema: WireSchema::ViewRelation,
                id: encode_id(basis.as_bytes()),
            }),
        );
        let encoded = crate::encode_command_body(&expected).expect("encode command");
        let decoded = crate::decode_command_body(&encoded).expect("decode command");
        assert_eq!(decoded, expected);
    }
}

#[test]
fn command_body_and_text_limits_are_enforced_by_the_shared_decoder() {
    let oversized_body = vec![b' '; crate::MAX_COMMAND_BODY + 1];
    assert!(crate::decode_command_body(&oversized_body).is_err());

    let request = CommandDto::new(
        42,
        Command::Resolve {
            text: "x".repeat(crate::MAX_COMMAND_TEXT + 1),
        },
    );
    let encoded = serde_json::to_vec(&request).expect("raw wire encoding");
    assert!(crate::decode_command_body(&encoded).is_err());
}

#[test]
fn graph_query_continuation_decodes_only_against_its_owner_cursor() {
    let source_root = view_state_root(&[]);
    let basis = Basis::new(source_root, object_version(b"graph-query-source"));
    let root = ViewRoot::empty_checked(
        view_key(b"graph-query-view"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, source_root, 7),
        capability(basis.object),
    )
    .expect("checked graph-query view");
    let owner = Cursor::for_view_root(&root);
    let request = crate::GraphQueryRequest::new(
        "{ Declaration { coordinate @output } }",
        std::collections::BTreeMap::new(),
        root.root(),
        QueryLimit::new(1).expect("one-row limit"),
    )
    .expect("graph query");
    let continuation = request
        .next_continuation(owner, 1)
        .expect("owner-bound continuation");
    let mut claims = certificate(&root)
        .claims
        .iter()
        .filter(|claim| !matches!(claim, WireClaim::Root { .. } | WireClaim::Cursor { .. }))
        .cloned()
        .collect::<Vec<_>>();
    claims.push(WireClaim::RootCommitment {
        schema: WireSchema::ViewRelation,
        id: encode_id(root.root().as_bytes()),
    });
    claims.push(WireClaim::KeyBytes {
        schema: WireSchema::ViewRecipe,
        id: encode_id(request.recipe().as_bytes()),
        value: request.recipe_preimage(),
    });
    claims.push(WireClaim::Cursor {
        recipe: encode_id(owner.recipe().as_bytes()),
        version: encode_id(owner.version().as_bytes()),
        branch: encode_id(owner.branch().as_bytes()),
        log: encode_id(owner.log().as_bytes()),
        schema: owner.schema(),
        root: encode_id(owner.root().as_bytes()),
        sequence: owner.sequence(),
    });
    let expected = CommandDto::new(
        43,
        Command::GraphQuery(request.with_continuation(continuation)),
    )
    .with_certificate(WireCertificate::from_claims(claims.into_boxed_slice()));
    let encoded = crate::encode_command_body(&expected).expect("encode opaque continuation");

    assert!(
        crate::decode_command_body(&encoded).is_err(),
        "standalone decoding must continue to require a canonical root"
    );
    assert_eq!(
        crate::decode_command_body_for_owner(&encoded, owner)
            .expect("owner admits its opaque continuation"),
        expected
    );
    let foreign_owner = owner.with_query_offset(0);
    let foreign_owner = Cursor::for_view(
        foreign_owner.recipe(),
        foreign_owner.version(),
        Frontier::new(
            foreign_owner.branch(),
            foreign_owner.log(),
            foreign_owner.schema(),
            view_state_root(&[("foreign".to_owned(), "root".to_owned())]),
            foreign_owner.sequence(),
        ),
    );
    assert!(crate::decode_command_body_for_owner(&encoded, foreign_owner).is_err());

    // The real client sends the fresh revision's owner claim. The query
    // recipe remains separate and binds all request arguments and page scope.
    let original: serde_json::Value = serde_json::from_slice(&encoded).expect("command JSON");
    for (path, replacement) in [
        (
            vec!["query"],
            serde_json::json!("{ Declaration { label @output } }"),
        ),
        (
            vec!["variables"],
            serde_json::json!({"name": {"kind": "string", "value": "changed"}}),
        ),
        (vec!["page", "limit"], serde_json::json!(2)),
        (
            vec!["page", "basis"],
            serde_json::json!(encode_id(foreign_owner.root().as_bytes())),
        ),
        (
            vec!["page", "continuation", "query_offset"],
            serde_json::json!(0),
        ),
        (
            vec!["page", "continuation", "sequence"],
            serde_json::json!(owner.sequence() + 1),
        ),
        (
            vec!["page", "continuation", "branch"],
            serde_json::json!(encode_id(crate::branch_key("forged").as_bytes())),
        ),
        (
            vec!["page", "continuation", "version"],
            serde_json::json!("00"),
        ),
    ] {
        let mut forged = original.clone();
        let mut field = &mut forged["command"]["data"];
        for part in &path {
            field = &mut field[*part];
        }
        *field = replacement;
        assert!(
            crate::decode_command_body_for_owner(
                &serde_json::to_vec(&forged).expect("forgery"),
                owner
            )
            .is_err(),
            "changed {path:?} must be refused"
        );
    }

    let advanced_owner = Cursor::for_view(
        owner.recipe(),
        owner.version(),
        Frontier::new(
            owner.branch(),
            owner.log(),
            owner.schema(),
            owner.root(),
            owner.sequence() + 1,
        ),
    );
    let mut same_view = original.clone();
    let claims = same_view["certificate"]["claims"]
        .as_array_mut()
        .expect("claims");
    let claim = claims
        .iter_mut()
        .find(|claim| claim["kind"] == "cursor")
        .expect("owner claim");
    claim["data"]["sequence"] = serde_json::json!(advanced_owner.sequence());
    let same_view = serde_json::to_vec(&same_view).expect("same view continuation");
    crate::decode_command_body_for_owner(&same_view, advanced_owner)
        .expect("same-view cold continuation with fresh authority");
}

fn reply_round_trip_fixtures() -> (Basis, ViewStateRoot, Vec<CommandReply>) {
    let source_root = view_state_root(&[]);
    let basis = Basis::new(source_root, object_version(b"source"));
    let root = ViewRoot::empty_checked(
        view_key(b"view"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, source_root, 0),
        capability(basis.object),
    )
    .expect("checked view root");
    let snapshot = ViewSnapshot {
        root: root.clone(),
        freshness: Freshness::Current,
        next: None,
        graph_relations: None,
        rich_graph: None,
    };
    let symbol = symbol_key("pkg::Thing");
    let package = crate::package_key("pkg");
    let excerpt = SourceExcerpt::captured("pub struct Thing;", SourceExcerptExtent::Complete)
        .expect("source excerpt");
    let document = Document::new(symbol, source_root, vec![Fragment::Text("docs".to_owned())])
        .with_location(SourceAvailability::Captured(
            SourceLocation::new("src/lib.rs", 7).expect("document source location"),
        ))
        .with_excerpt(excerpt.clone());
    let outline = Outline::new(
        package,
        source_root,
        OutlineNode {
            symbol,
            children: Box::new([]),
        },
    );
    let rows = [
        SourceAvailability::Captured(
            SourceLocation::new("src/lib.rs", 7).expect("captured source location"),
        ),
        SourceAvailability::NotCaptured,
        SourceAvailability::NotHydrated,
        SourceAvailability::Unconfigured,
        SourceAvailability::stale_file("src/lib.rs").expect("stale file"),
    ]
    .map(|source| {
        let mut row = Row::new(RowId::Symbol(symbol), basis, "Thing");
        row.source = source;
        row.excerpt = excerpt.clone();
        row
    })
    .into_iter()
    .collect::<Vec<_>>()
    .into_boxed_slice();
    let intent = crate::intent_id("add", b"pkg");
    let replies = [
        CommandReply::Packages(snapshot.clone()),
        CommandReply::ProjectionPage(crate::ProjectionPage {
            outline_order: None,
            snapshot: snapshot.clone(),
            terminal: crate::PageTerminal::Complete,
        }),
        CommandReply::Added(intent),
        CommandReply::Removed(intent),
        CommandReply::Document(document.clone()),
        CommandReply::Page(document),
        CommandReply::Outline(outline),
        CommandReply::Names(snapshot.clone()),
        CommandReply::Resolved(rows),
        CommandReply::Search(snapshot.clone()),
        CommandReply::Graph(snapshot),
        CommandReply::Health(root.clone()),
        CommandReply::Error("error".to_owned()),
    ];
    (basis, source_root, replies.into_iter().collect())
}

#[test]
fn reply_and_view_dtos_round_trip_every_reply_variant() {
    let (basis, source_root, replies) = reply_round_trip_fixtures();
    for reply in replies {
        let dto = match reply {
            CommandReply::Health(root) => {
                ReplyDto::health(12, root.clone(), Cursor::for_view_root(&root))
                    .with_certificate(certificate(&root))
            }
            reply => ReplyDto::new(12, reply),
        };
        let encoded = serde_json::to_vec(&dto).expect("encode reply");
        if !matches!(&dto.reply, CommandReply::Error(_) | CommandReply::Health(_)) {
            assert!(serde_json::from_slice::<ReplyDto>(&encoded).is_err());
        }
        let decoded = ReplyDto::decode_against(&encoded, &dto).expect("decode reply");
        assert_eq!(decoded, dto);
    }

    let view_root = ViewRoot::empty_checked(
        view_key(b"view-dto"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, source_root, 0),
        capability(basis.object),
    )
    .expect("checked view root");
    let center = crate::GraphNodeId::for_symbol(symbol_key("pkg::Thing"));
    let revision = crate::RichGraphRevision::new(
        view_root.root().to_bytes(),
        crate::SemanticGenerationId::new([7; 32]),
        [8; 32],
    );
    let mut graph = crate::RichGraphBuilder::new(revision, center);
    graph
        .add_node(
            crate::RichGraphNode::new(
                center,
                "Thing",
                Some("pkg::Thing".to_owned()),
                Some("struct".to_owned()),
                crate::GraphAvailability::Ready,
                None,
            )
            .expect("graph node"),
        )
        .expect("graph center");
    let rich_graph = graph.finish().expect("rich graph");
    let view = ViewDto::new(
        13,
        ViewSnapshot {
            root: view_root,
            freshness: Freshness::Stale {
                observed: source_root,
            },
            next: None,
            graph_relations: None,
            rich_graph: Some(rich_graph),
        },
    );
    let encoded = serde_json::to_vec(&view).expect("encode view");
    assert!(serde_json::from_slice::<ViewDto>(&encoded).is_err());
    let decoded = ViewDto::decode_against(&encoded, &view).expect("decode view");
    assert_eq!(decoded, view);
}

#[test]
fn semantic_shape_reply_round_trips_exact_image_at_depth_limit_and_rejects_bad_id() {
    use backend_semantic::vocabulary::{JavaRelease, LanguageProfile, RustEdition};
    use backend_version::{ArtifactId, IrSemanticImageDomain, IrSemanticImageEncoding};

    let source_root = view_state_root(&[]);
    let basis = Basis::new(source_root, object_version(b"semantic-shape-source"));
    let symbol = symbol_key("pkg::shape_fixture::call");
    let base = ViewRoot::empty_checked(
        view_key(b"semantic-shape-view"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, source_root, 0),
        capability(basis.object),
    )
    .expect("checked view root");
    let prepared = base
        .prepare(
            ViewDelta::Upsert {
                row: Row::new(RowId::Symbol(symbol), basis, "call"),
            },
            capability(basis.object),
        )
        .expect("prepare visible declaration row");
    let (root, _) = base.commit(prepared).expect("commit visible row");
    let coordinate = crate::PackageCoordinate::parse("pkg:cargo/shape-fixture@1.0.0")
        .expect("fixture package coordinate");
    let profile = LanguageProfile::Rust(RustEdition::Rust2021);
    let source = crate::SemanticVersionRecord {
        package: crate::PackageReference::Purl(coordinate.clone()),
        coordinate,
        profile: crate::SemanticLanguageProfile::new(profile),
        generation: crate::SemanticGenerationId::new([31; 32]),
        generation_root: [32; 32],
        dependency_set: [33; 32],
        manifest: [34; 32],
        artifacts: 1,
        semantic_bytes: 4096,
        complete: true,
        selected: true,
        freshness: crate::SemanticVersionFreshness::Current {
            input_digest: [35; 32],
        },
        history_status: Default::default(),
        selected_source_frontier: None,
    };
    let request = crate::SemanticShapeRequest::new(
        root.root(),
        source.clone(),
        crate::SemanticSourceEpoch {
            source_relation_root: [0x21; 32],
            semantic_relation_root: [0x22; 32],
        },
        vec![crate::SymbolAddress::selected(symbol)].into_boxed_slice(),
        crate::SemanticShapeBudget::new(4096, crate::MAX_SEMANTIC_SHAPE_BYTES as u32)
            .expect("hard limits are valid"),
    )
    .expect("shape request");

    // The payload preserves exact compiler atom spellings through the complete
    // reply protocol, including JSON escapes. One optional edge below the
    // object reaches the same hard depth limit as the callable chain.
    let atom_texts = ["", " ", "\0", "λ雪", " padded "]
        .map(|value| crate::SourceAtomText::new(value).expect("exact bounded atom"));
    let property_keys = [
        crate::SemanticPropertyKey::Named(atom_texts[0].clone()),
        crate::SemanticPropertyKey::Private(atom_texts[1].clone()),
        crate::SemanticPropertyKey::Numeric(atom_texts[2].clone()),
        crate::SemanticPropertyKey::Named(atom_texts[3].clone()),
        crate::SemanticPropertyKey::Private(atom_texts[4].clone()),
    ];
    let mut object_members = property_keys
        .into_iter()
        .zip(atom_texts.iter().cloned())
        .map(|(key, value)| crate::SemanticObjectMember::Property {
            key,
            ty: crate::SemanticTypeFact::Known(crate::SemanticTypeExpr::Literal(
                crate::SemanticLiteral::String(value),
            )),
            optional: false,
            readonly: false,
        })
        .collect::<Vec<_>>();
    object_members.push(crate::SemanticObjectMember::Property {
        key: crate::SemanticPropertyKey::Named(
            crate::SourceAtomText::new("deep").expect("exact bounded key"),
        ),
        ty: crate::SemanticTypeFact::Known(crate::SemanticTypeExpr::Optional(Box::new(
            crate::SemanticTypeFact::Unavailable(crate::SemanticTypeUnavailable::DepthBudget),
        ))),
        optional: false,
        readonly: false,
    });
    let mut nested = crate::SemanticTypeFact::Known(crate::SemanticTypeExpr::Object(
        object_members.into_boxed_slice(),
    ));
    for _ in 0..crate::MAX_SEMANTIC_SHAPE_DEPTH - 2 {
        nested = crate::SemanticTypeFact::Known(crate::SemanticTypeExpr::Function(Box::new(
            crate::SemanticCallableShape {
                parameters: vec![crate::SemanticTypeElement {
                    label: None,
                    kind: backend_semantic::ir::TupleElementKind::Required,
                    ty: nested,
                }]
                .into_boxed_slice(),
                results: Box::new([]),
                carrier_bindings: crate::SemanticCallableCarrierBindings::Unavailable,
                abi: None,
                variadic: backend_semantic::ir::FunctionVariadicForm::None,
                unsafe_: false,
            },
        )));
    }
    let image_identity =
        ArtifactId::<IrSemanticImageEncoding, IrSemanticImageDomain>::from_encoded_bytes(
            b"semantic shape exact image fixture",
        );
    let origin = crate::SemanticShapeSourceOrigin {
        source: crate::SemanticShapeSelection::from_selected(&source)
            .expect("selected source witness"),
        selection_root: [36; 32],
        semantic_image_bytes: crate::SemanticImagePayloadBytes::new(4096)
            .expect("fixture aggregate image extent"),
        image: Some(crate::SemanticShapeImageOrigin {
            image: crate::interface::SemanticImageAuthority {
                identity: image_identity,
                byte_len: 512,
            },
            profile,
        }),
    };
    let batch = crate::SemanticShapeBatch {
        basis: request.basis(),
        entries: vec![crate::SemanticShapeEntry {
            symbol: crate::SymbolAddress::selected(symbol),
            identity: Some(crate::SemanticDeclarationIdentity {
                family: [37; 16],
                variant: [38; 16],
            }),
            origin: Some(origin.clone()),
            fact: crate::SemanticShapeFact::Available {
                shape: crate::SemanticDeclarationShape::Typed(nested),
                language: crate::SemanticShapeLanguageFacts::CommonOnly { profile },
            },
        }]
        .into_boxed_slice(),
    };
    batch.admit_against(&request).expect("bounded shape batch");

    let source_key = crate::semantic_shape_source_key(&origin);
    let batch_key = crate::semantic_shape_batch_key(&batch).expect("shape batch commitment");
    let valid_certificate = certificate(&root)
        .with_claim(WireClaim::RootCommitment {
            schema: WireSchema::ViewRelation,
            id: encode_id(batch.basis.as_bytes()),
        })
        .with_claim(WireClaim::KeyCommitment {
            schema: WireSchema::Symbol,
            id: encode_id(symbol.as_bytes()),
        })
        .with_claim(WireClaim::KeyBytes {
            schema: WireSchema::SemanticShapeSource,
            id: encode_id(source_key.as_bytes()),
            value: crate::semantic_shape_source_preimage(&origin).into_boxed_slice(),
        })
        .with_claim(WireClaim::KeyCommitment {
            schema: WireSchema::SemanticShapeBatch,
            id: encode_id(batch_key.as_bytes()),
        });
    let command = CommandDto::new(44, Command::SemanticShapes(request.clone()));
    let reply = ReplyDto::new(44, CommandReply::SemanticShapes(batch.clone()))
        .with_certificate(valid_certificate);
    let exported = crate::SemanticShapeExport::from_admitted_reply(&reply, &request)
        .expect("certificate-admitted shape export");
    let json = exported.encode_bounded_json().expect("bounded egress");
    let view: crate::SemanticShapeExport =
        serde_json::from_slice(&json).expect("egress view roundtrip");
    assert_eq!(view, exported);
    let value: serde_json::Value = serde_json::from_slice(&json).expect("egress JSON");
    assert_eq!(value["schema"], 1);
    assert_eq!(
        value["source"]["generation"],
        serde_json::json!(vec![31u8; 32])
    );
    assert_eq!(value["max_nodes"], 4096);
    // A display packet has no authority certificate, but contradictory source,
    // image, selector and caller-budget statements must still fail decoding.
    for (label, pointer, replacement) in [
        ("caller nodes", "/max_nodes", serde_json::json!(1)),
        ("caller bytes", "/max_bytes", serde_json::json!(1)),
        (
            "source generation",
            "/source/generation",
            serde_json::json!(vec![90u8; 32]),
        ),
        (
            "origin root",
            "/batch/entries/0/origin/selection_root",
            serde_json::json!(vec![90u8; 32]),
        ),
        (
            "source commitment",
            "/batch/entries/0/origin/source_commitment",
            serde_json::json!("00".repeat(32)),
        ),
        (
            "image identity",
            "/batch/entries/0/origin/image/image_identity",
            serde_json::json!("91".repeat(32)),
        ),
        (
            "image extent",
            "/batch/entries/0/origin/image/byte_len",
            serde_json::json!(4097),
        ),
        (
            "image profile",
            "/batch/entries/0/origin/image/profile",
            serde_json::json!([2, 0]),
        ),
        (
            "missing provenance",
            "/batch/entries/0/origin",
            serde_json::Value::Null,
        ),
        (
            "missing identity",
            "/batch/entries/0/identity",
            serde_json::Value::Null,
        ),
        (
            "zero selector",
            "/batch/entries/0/symbol/id",
            serde_json::json!("00".repeat(32)),
        ),
        (
            "malformed basis",
            "/batch/basis",
            serde_json::json!("short"),
        ),
    ] {
        let mut malformed = value.clone();
        *malformed.pointer_mut(pointer).expect("fixture field") = replacement;
        assert!(
            serde_json::from_value::<crate::SemanticShapeExport>(malformed).is_err(),
            "{label}"
        );
    }
    let mut duplicated = value.clone();
    duplicated["batch"]["entries"]
        .as_array_mut()
        .expect("entries")
        .push(value["batch"]["entries"][0].clone());
    assert!(serde_json::from_value::<crate::SemanticShapeExport>(duplicated).is_err());
    let mut roots = value.clone();
    let mut second = value["batch"]["entries"][0].clone();
    second["symbol"]["id"] = serde_json::json!("93".repeat(32));
    second["origin"]["selection_root"] = serde_json::json!(vec![94u8; 32]);
    let mut distinct_origin = origin.clone();
    distinct_origin.selection_root = [94; 32];
    second["origin"]["source_commitment"] = serde_json::json!(encode_id(
        crate::semantic_shape_source_key(&distinct_origin).as_bytes()
    ));
    roots["batch"]["entries"]
        .as_array_mut()
        .expect("entries")
        .push(second);
    assert!(
        serde_json::from_value::<crate::SemanticShapeExport>(roots).is_err(),
        "each valid source commitment cannot conceal conflicting workspace roots"
    );

    // The view basis and workspace selection root are different identity planes.
    // A well-formed changed display basis cannot be authenticated by its decoder.
    let mut display_only = value.clone();
    display_only["batch"]["basis"] = serde_json::json!("92".repeat(32));
    assert!(serde_json::from_value::<crate::SemanticShapeExport>(display_only).is_ok());
    let other_basis_request = crate::SemanticShapeRequest::new(
        view_state_root(&[("different".to_owned(), "view".to_owned())]),
        source.clone(),
        request.source_epoch(),
        request.symbols().to_vec().into_boxed_slice(),
        request.budget(),
    )
    .expect("different request basis");
    assert!(crate::SemanticShapeExport::from_admitted_reply(&reply, &other_basis_request).is_err());

    assert_eq!(
        value["batch"]["entries"][0]["origin"]["selection_root"],
        serde_json::json!(vec![36u8; 32])
    );
    assert!(
        crate::SemanticShapeExport::from_admitted_reply(
            &ReplyDto::new(44, CommandReply::SemanticShapes(batch.clone())),
            &request
        )
        .is_err(),
        "certificate-free typed values must not become exported admitted products"
    );
    let mut wrong_origin = batch.clone();
    wrong_origin.entries[0]
        .origin
        .as_mut()
        .expect("origin")
        .selection_root = [99; 32];
    assert!(
        crate::SemanticShapeExport::from_admitted_reply(
            &ReplyDto::new(44, CommandReply::SemanticShapes(wrong_origin))
                .with_certificate(reply.certificate().expect("certificate").clone()),
            &request
        )
        .is_err()
    );
    let encoded = serde_json::to_vec(&reply).expect("encode complete reply DTO");
    let decoded = crate::decode_reply_body(&encoded).expect("decode complete reply DTO");
    admit_reply(&command, &decoded).expect("admit exact request and reply");
    let CommandReply::SemanticShapes(decoded_batch) = decoded.reply else {
        panic!("semantic shape reply changed variant");
    };
    assert_eq!(decoded_batch, batch);
    assert_eq!(
        decoded_batch.entries[0]
            .origin
            .as_ref()
            .and_then(|origin| origin.image)
            .map(|image| image.image.identity),
        Some(image_identity),
        "wire decoding must preserve the exact raw artifact ID, not hash it again"
    );

    let mut wrong_language = batch.clone();
    let crate::SemanticShapeFact::Available { shape, .. } = wrong_language.entries[0].fact.clone()
    else {
        panic!("fixture shape is available");
    };
    wrong_language.entries[0].fact = crate::SemanticShapeFact::Available {
        shape,
        language: crate::SemanticShapeLanguageFacts::CommonOnly {
            profile: LanguageProfile::Java(JavaRelease::Java17),
        },
    };
    let wrong_language_key =
        crate::semantic_shape_batch_key(&wrong_language).expect("changed language payload key");
    let wrong_language_certificate = certificate(&root)
        .with_claim(WireClaim::RootCommitment {
            schema: WireSchema::ViewRelation,
            id: encode_id(wrong_language.basis.as_bytes()),
        })
        .with_claim(WireClaim::KeyCommitment {
            schema: WireSchema::Symbol,
            id: encode_id(symbol.as_bytes()),
        })
        .with_claim(WireClaim::KeyBytes {
            schema: WireSchema::SemanticShapeSource,
            id: encode_id(source_key.as_bytes()),
            value: crate::semantic_shape_source_preimage(&origin).into_boxed_slice(),
        })
        .with_claim(WireClaim::KeyCommitment {
            schema: WireSchema::SemanticShapeBatch,
            id: encode_id(wrong_language_key.as_bytes()),
        });
    let wrong_language_reply = ReplyDto::new(44, CommandReply::SemanticShapes(wrong_language))
        .with_certificate(wrong_language_certificate);
    let wrong_language_encoded =
        serde_json::to_vec(&wrong_language_reply).expect("encode profile mismatch fixture");
    let wrong_language_decoded =
        crate::decode_reply_body(&wrong_language_encoded).expect("wire attestation is valid");
    assert!(
        admit_reply(&command, &wrong_language_decoded).is_err(),
        "exact request admission must reject an otherwise valid payload attested for another language profile"
    );

    fn mutate_depth_terminal(value: &mut serde_json::Value) -> bool {
        if value.get("state").and_then(serde_json::Value::as_str) == Some("unavailable")
            && value.get("data").and_then(serde_json::Value::as_str) == Some("depth_budget")
        {
            value["data"] = serde_json::json!("node_budget");
            return true;
        }
        match value {
            serde_json::Value::Array(items) => items.iter_mut().any(mutate_depth_terminal),
            serde_json::Value::Object(fields) => fields.values_mut().any(mutate_depth_terminal),
            _ => false,
        }
    }

    let mut mutated_fact: serde_json::Value =
        serde_json::from_slice(&encoded).expect("reply envelope JSON");
    let original_json: serde_json::Value =
        serde_json::from_slice(&encoded).expect("reply envelope JSON");
    assert!(mutate_depth_terminal(
        &mut mutated_fact["reply"]["data"]["entries"][0]["fact"]
    ));
    assert_ne!(
        mutated_fact, original_json,
        "mutation must change a valid nested shape fact"
    );
    let mutated_fact = serde_json::to_vec(&mutated_fact).expect("encode fact mutation");
    assert!(
        crate::decode_reply_body(&mutated_fact).is_err(),
        "a full reply decode must reject a changed type fact with the original certificate"
    );

    let mut malformed: serde_json::Value =
        serde_json::from_slice(&encoded).expect("reply envelope JSON");
    let image_id = malformed["reply"]["data"]["entries"][0]["origin"]["image"]["image_identity"]
        .as_str()
        .expect("image identity string");
    let mut raw_id = crate::decode_id(image_id).expect("well-formed image ID");
    raw_id[0] ^= 0x80;
    malformed["reply"]["data"]["entries"][0]["origin"]["image"]["image_identity"] =
        serde_json::Value::String(encode_id(&raw_id));
    let malformed = serde_json::to_vec(&malformed).expect("encode malformed reply");
    assert!(
        crate::decode_reply_body(&malformed).is_err(),
        "a full reply decode must reject an image ID with the wrong authority bytes"
    );
}

#[test]
fn search_reply_round_trips_semantic_lane_status_without_rewriting_lexical_rows() {
    let (_, _, replies) = reply_round_trip_fixtures();
    let snapshot = replies
        .into_iter()
        .find_map(|reply| match reply {
            CommandReply::Search(snapshot) => Some(snapshot),
            _ => None,
        })
        .expect("search fixture");
    let status = crate::SemanticSearchStatus::Unavailable {
        reason: crate::SemanticSearchReason::ProviderUnavailable,
    };
    let dto = ReplyDto::new(13, CommandReply::Search(snapshot.clone()))
        .with_semantic_search_status(status);
    let encoded = serde_json::to_vec(&dto).expect("encode search reply");
    let envelope: serde_json::Value = serde_json::from_slice(&encoded).expect("reply envelope");
    assert_eq!(envelope["version"], serde_json::json!(DTO_VERSION));
    assert_eq!(
        envelope["semantic_search"],
        serde_json::json!({
            "status": "unavailable",
            "reason": "provider_unavailable"
        })
    );
    let decoded = ReplyDto::decode_against(&encoded, &dto).expect("decode search status");
    assert_eq!(decoded.semantic_search_status(), Some(status));
    let CommandReply::Search(decoded_snapshot) = decoded.reply else {
        panic!("search reply shape changed")
    };
    assert_eq!(decoded_snapshot, snapshot);

    let misplaced =
        ReplyDto::new(14, CommandReply::Names(snapshot)).with_semantic_search_status(status);
    assert!(serde_json::to_vec(&misplaced).is_err());
}

#[test]
fn document_and_outline_wire_bind_the_complete_source_basis() {
    let root = view_state_root(&[]);
    let source = Basis::with_context(
        root,
        object_version(b"source-object"),
        crate::branch_key("feature"),
        crate::log_key("semantic"),
        7,
    );
    let symbol = symbol_key("pkg::Thing");
    let package = crate::package_key("pkg");
    let document = Document::new(symbol, root, vec![Fragment::Text("docs".to_owned())])
        .with_source_basis(source);
    let outline = Outline::new(
        package,
        root,
        OutlineNode {
            symbol,
            children: Box::new([]),
        },
    )
    .with_source_basis(source);

    let document_reply = ReplyDto::new(20, CommandReply::Document(document.clone()));
    let encoded = serde_json::to_vec(&document_reply).expect("encode document");
    assert_eq!(
        ReplyDto::decode_against(&encoded, &document_reply).expect("decode document"),
        document_reply
    );
    let mut forged: serde_json::Value = serde_json::from_slice(&encoded).expect("document JSON");
    forged["reply"]["data"]["source"]["branch"] =
        serde_json::json!(encode_id(crate::branch_key("other").as_bytes()));
    assert!(
        ReplyDto::decode_against(
            serde_json::to_vec(&forged)
                .expect("encode forged")
                .as_slice(),
            &document_reply,
        )
        .is_err()
    );

    let outline_reply = ReplyDto::new(21, CommandReply::Outline(outline.clone()));
    let encoded = serde_json::to_vec(&outline_reply).expect("encode outline");
    assert_eq!(
        ReplyDto::decode_against(&encoded, &outline_reply).expect("decode outline"),
        outline_reply
    );

    let document_command = CommandDto::new(
        22,
        Command::Document(DocumentQuery::new(symbol, root).with_source_basis(source)),
    );
    let encoded = serde_json::to_vec(&document_command).expect("encode document command");
    assert_eq!(
        CommandDto::decode_against(&encoded, &document_command).expect("decode command"),
        document_command
    );
    let outline_command = CommandDto::new(
        23,
        Command::Outline(OutlineQuery::new(package, root).with_source_basis(source)),
    );
    let encoded = serde_json::to_vec(&outline_command).expect("encode outline command");
    assert_eq!(
        CommandDto::decode_against(&encoded, &outline_command).expect("decode command"),
        outline_command
    );
}

#[test]
fn dto_versions_and_outer_fields_are_strict() {
    let command = CommandDto::new(1, Command::Health);
    let mut command_json = serde_json::to_value(&command).expect("command JSON");
    command_json["version"] = serde_json::json!(DTO_VERSION + 1);
    assert!(serde_json::from_value::<CommandDto>(command_json).is_err());

    let mut old_command_json = serde_json::to_value(&command).expect("command JSON");
    old_command_json["version"] = serde_json::json!(DTO_VERSION - 1);
    assert!(serde_json::from_value::<CommandDto>(old_command_json).is_err());

    let reply = ReplyDto::error(2, "error");
    for unsupported in [DTO_VERSION - 1, DTO_VERSION + 1] {
        let mut reply_json = serde_json::to_value(&reply).expect("reply JSON");
        reply_json["version"] = serde_json::json!(unsupported);
        let error = serde_json::from_value::<ReplyDto>(reply_json)
            .expect_err("a different build's protocol is refused")
            .to_string();
        assert!(error.contains(&format!("reply DTO version {unsupported}")));
        assert!(error.contains(&format!("this build supports {DTO_VERSION}")));
        assert!(error.contains("same build"));
        assert!(error.contains("Install matching Nudox CLI/MCP/GUI and owner components"));
    }
    let mut reply_json = serde_json::to_value(&reply).expect("reply JSON");
    reply_json["extra"] = serde_json::json!(true);
    assert!(serde_json::from_value::<ReplyDto>(reply_json).is_err());
    let mut reply_json = serde_json::to_value(&reply).expect("reply JSON");
    reply_json["reply"]["data"]["extra"] = serde_json::json!(true);
    assert!(serde_json::from_value::<ReplyDto>(reply_json).is_err());

    let event = EventDto::new(
        Cursor::new(),
        CursorEvent::Intent {
            id: crate::intent_id("event", b"payload"),
        },
    );
    let mut event_json = serde_json::to_value(&event).expect("event JSON");
    event_json["version"] = serde_json::json!(DTO_VERSION + 1);
    assert!(serde_json::from_value::<EventDto>(event_json).is_err());
    let mut event_json = serde_json::to_value(&event).expect("event JSON");
    event_json["extra"] = serde_json::json!(true);
    assert!(serde_json::from_value::<EventDto>(event_json).is_err());
    let mut event_json = serde_json::to_value(&event).expect("event JSON");
    event_json["event"]["data"]["extra"] = serde_json::json!(true);
    assert!(serde_json::from_value::<EventDto>(event_json).is_err());
}

#[test]
fn compiler_outcome_selector_cutover_refuses_dto32_before_payload_shape() {
    assert_eq!(DTO_VERSION, 39);
    let command = CommandDto::new(
        1,
        Command::Surface(crate::SurfaceCommand::PackageProfile {
            package: crate::PackageReference::parse("/compiler-outcomes").expect("local"),
            compiler_outcome: Some(crate::PackageCompilerOutcomeRead::Diagnostic(
                crate::PackageCompilerOutcomeSelector {
                    basis: [7; 32],
                    profile: crate::SemanticLanguageProfile::from_name("python").expect("profile"),
                },
            )),
        }),
    );
    let mut value = serde_json::to_value(command).expect("current typed request");
    let bytes = serde_json::to_vec(&value).expect("current encoding");
    assert!(
        super::codec::check_version_header_against(&bytes, "command", 32)
            .expect_err("old peer rejects before decoding new field")
            .contains("command DTO version 39")
    );
    value["version"] = serde_json::json!(32);
    let error = crate::decode_command_body(&serde_json::to_vec(&value).expect("old envelope"))
        .expect_err("current peer refuses old cohort");
    assert!(error.contains("command DTO version 32"));
    assert!(!error.contains("unknown field"));
    assert!(
        crate::JournalViewGrammarV3::from_checked_container(3)
            .expect("unchanged persisted grammar")
            .check_envelope_version(32)
            .is_ok()
    );
}

#[test]
fn published_package_coverage_refuses_dto37_before_payload_admission() {
    assert_eq!(DTO_VERSION, 39);
    let request = CommandDto::new(
        73,
        Command::Surface(crate::SurfaceCommand::PackageCoverageCurrent {
            package: crate::PackageReference::parse("/published-coverage")
                .expect("local package"),
            view_root: [74; 32],
        }),
    );
    let mut value = serde_json::to_value(request).expect("typed DTO39 request");
    let current = serde_json::to_vec(&value).expect("current encoding");
    let old_peer = super::codec::check_version_header_against(&current, "command", 37)
        .expect_err("DTO37 peer refuses the owner-bound coverage command");
    assert!(old_peer.contains("command DTO version 39"));
    assert!(old_peer.contains("this build supports 37"));

    value["version"] = serde_json::json!(37);
    value["command"] = serde_json::json!({"new_coverage_shape": true});
    let old = serde_json::to_vec(&value).expect("old-cohort adversary");
    let error = crate::decode_command_body(&old)
        .expect_err("current client rejects DTO37 before the unknown payload shape");
    assert!(error.contains("command DTO version 37"));
    assert!(error.contains("this build supports 39"));
    assert!(!error.contains("new_coverage_shape"));
}

#[test]
fn usability_batch_read_wire_extent_is_versioned_and_older_records_stay_honest() {
    use crate::{DeclarationReadExtent, DeclarationRecord, ProductText, SurfaceReply};
    let legacy_record = serde_json::json!({
        "label": "WeekService", "stable_id": null, "signature": "class WeekService {}"
    });
    let decoded: DeclarationRecord =
        serde_json::from_value(legacy_record.clone()).expect("older record omits extent");
    assert_eq!(decoded.extent, None, "absence cannot claim complete source");
    assert_eq!(
        serde_json::to_value(&decoded).expect("old shape"),
        legacy_record
    );
    for extent in [
        DeclarationReadExtent::Complete,
        DeclarationReadExtent::Truncated,
    ] {
        let record = DeclarationRecord {
            label: ProductText::new("WeekService").expect("label"),
            stable_id: Some([8; 32]),
            signature: Some(ProductText::new("class WeekService {").expect("source")),
            extent: Some(extent),
        };
        let reply = ReplyDto::new(
            41,
            CommandReply::Surface(SurfaceReply::Read(Box::new([record]))),
        );
        let encoded = serde_json::to_vec(&reply).expect("current wire read");
        let decoded = crate::wire::decode_reply_body(&encoded).expect("strict current decode");
        assert_eq!(decoded, reply);
        assert!(crate::reply_memory_bound(&reply) >= encoded.len());
        let mut old: serde_json::Value = serde_json::from_slice(&encoded).expect("read JSON");
        assert_eq!(old["version"], DTO_VERSION);
        old["version"] = serde_json::json!(26);
        let error = serde_json::from_value::<ReplyDto>(old)
            .expect_err("old live DTO refused")
            .to_string();
        assert!(error.contains("reply DTO version 26"));
        assert!(error.contains(&format!("this build supports {DTO_VERSION}")));
    }
    let grammar = crate::JournalViewGrammarV3::from_checked_container(3).expect("checked grammar");
    for historical in 21..=38 {
        grammar
            .check_envelope_version(historical)
            .expect("maintained journal law");
    }
    grammar
        .check_envelope_version(DTO_VERSION)
        .expect("current journal law");
    assert!(grammar.check_envelope_version(20).is_err());
    assert!(grammar.check_envelope_version(DTO_VERSION + 1).is_err());
    // The shared certificate supplies these exact recipe and object preimages.
    // Prove its admission before changing only the historical envelope version.
    let basis = Basis::new(view_state_root(&[]), object_version(b"source"));
    let root = ViewRoot::empty_checked(
        view_key(b"view"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
        capability(basis.object),
    )
    .expect("checked empty producer snapshot");
    let snapshot = ViewDto::new(
        0,
        ViewSnapshot {
            root: root.clone(),
            freshness: Freshness::Current,
            next: None,
            graph_relations: None,
            rich_graph: None,
        },
    )
    .with_certificate(certificate(&root));
    let current = serde_json::to_vec(&snapshot).expect("current snapshot wire");
    let admitted = ViewDto::decode_with_certificate(&current, Some(capability(basis.object)))
        .expect("strict current snapshot certificate");
    assert_eq!(admitted.snapshot.root, root);
    let mut historical = serde_json::to_value(&snapshot).expect("current snapshot JSON");
    historical["version"] = serde_json::json!(26);
    let bytes = serde_json::to_vec(&historical).expect("historical snapshot JSON");
    let restored = grammar
        .decode_snapshot(&bytes, capability(basis.object))
        .expect("strict historical26 journal snapshot");
    assert_eq!(restored.snapshot.root, root);
    assert!(
        ViewDto::decode_with_certificate(&bytes, Some(capability(basis.object))).is_err(),
        "journal compatibility cannot authorize an old live peer"
    );

    for (stable_id, signature) in [
        (None, Some(ProductText::new("source").expect("text"))),
        (Some([8; 32]), None),
    ] {
        let invalid = SurfaceReply::Read(Box::new([DeclarationRecord {
            label: ProductText::new("WeekService").expect("label"),
            stable_id,
            signature,
            extent: Some(DeclarationReadExtent::Truncated),
        }]));
        assert_eq!(
            invalid.admit(crate::CommandId::Read),
            Err(crate::ProductAdmissionError::DeclarationReadShape)
        );
    }
}

#[test]
fn subscription_projection_rejects_cursor_mutation_and_unknown_fields() {
    let previous = Cursor::new();
    let subscription = SubscriptionDto::try_events(previous, previous, Vec::<EventDto>::new())
        .expect("empty subscription");
    let encoded = serde_json::to_vec(&subscription).expect("encode subscription");
    let decoded = SubscriptionDto::decode(&encoded, previous, None).expect("decode subscription");
    assert!(matches!(decoded, crate::CursorRead::Events { events, .. } if events.is_empty()));

    let mut forged: serde_json::Value = serde_json::from_slice(&encoded).expect("value");
    forged["cursor"]["root"] = serde_json::json!(encode_id(
        view_state_root(&[("forged".to_owned(), "root".to_owned())]).as_bytes()
    ));
    assert!(
        SubscriptionDto::decode(
            serde_json::to_vec(&forged)
                .expect("encode forged")
                .as_slice(),
            previous,
            None,
        )
        .is_err()
    );

    forged["cursor"]["root"] = serde_json::json!(encode_id(previous.root().as_bytes()));
    forged["unexpected"] = serde_json::json!(true);
    assert!(
        SubscriptionDto::decode(
            serde_json::to_vec(&forged)
                .expect("encode unknown")
                .as_slice(),
            previous,
            None,
        )
        .is_err()
    );
}

#[test]
fn subscription_projection_rejects_replayed_reset_and_incomplete_producer_root() {
    let source_root = view_state_root(&[]);
    let basis = Basis::new(source_root, object_version(b"source"));
    let complete = ViewRoot::empty_checked(
        view_key(b"view"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, source_root, 0),
        capability(basis.object),
    )
    .expect("complete root");
    let next = Cursor::for_view(
        complete.recipe(),
        complete.version(),
        Frontier::new(
            complete.frontier().branch,
            complete.frontier().log,
            complete.frontier().schema,
            complete.root(),
            1,
        ),
    );
    let view = ViewDto::new(
        0,
        ViewSnapshot {
            root: complete.clone(),
            freshness: Freshness::Current,
            next: None,
            graph_relations: None,
            rich_graph: None,
        },
    );
    let reset = SubscriptionDto::try_reset(
        Cursor::for_view(
            complete.recipe(),
            complete.version(),
            Frontier::new(
                complete.frontier().branch,
                complete.frontier().log,
                complete.frontier().schema,
                complete.root(),
                0,
            ),
        ),
        next,
        view,
        crate::CursorResetReason::Gap,
    )
    .expect("reset");
    assert!(reset.clone().into_read(next).is_err());

    let incomplete = ViewRoot::new_incomplete(
        complete.recipe(),
        complete.basis(),
        complete.frontier(),
        Vec::new(),
        Vec::new(),
    )
    .expect("incomplete root");
    let incomplete_view = ViewDto::new(
        0,
        ViewSnapshot {
            root: incomplete,
            freshness: Freshness::Current,
            next: None,
            graph_relations: None,
            rich_graph: None,
        },
    );
    assert!(
        SubscriptionDto::try_reset(
            Cursor::new(),
            next,
            incomplete_view,
            crate::CursorResetReason::Gap,
        )
        .is_err()
    );
}

#[test]
fn complete_view_projection_is_the_single_root_cursor_admission() {
    let source_root = view_state_root(&[]);
    let basis = Basis::new(source_root, object_version(b"source"));
    let root = ViewRoot::empty_checked(
        view_key(b"view"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, source_root, 0),
        capability(basis.object),
    )
    .expect("complete root");
    let cursor = Cursor::for_view(
        root.recipe(),
        root.version(),
        Frontier::new(
            root.frontier().branch,
            root.frontier().log,
            root.frontier().schema,
            root.root(),
            0,
        ),
    );
    assert!(CompleteViewProjection::admit(root.clone(), cursor).is_ok());
    let derived = CompleteViewProjection::from_root(root.clone()).expect("derived projection");
    assert_eq!(derived.cursor().root(), root.root());
    assert_eq!(derived.cursor().sequence(), root.frontier().sequence);
    let wrong = Cursor::for_view(
        cursor.recipe(),
        cursor.version(),
        Frontier::new(
            cursor.branch(),
            cursor.log(),
            cursor.schema(),
            view_state_root(&[("wrong".to_owned(), "root".to_owned())]),
            cursor.sequence(),
        ),
    );
    assert!(CompleteViewProjection::admit(root, wrong).is_err());
}

#[test]
fn forged_same_scope_certificate_cannot_mint_complete_coverage() {
    let source_root = view_state_root(&[]);
    let basis = Basis::new(source_root, object_version(b"source"));
    let capability = capability(basis.object);
    let root = ViewRoot::empty_checked(
        view_key(b"view"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, source_root, 0),
        capability.clone(),
    )
    .expect("complete root");
    let view = ViewDto::new(
        0,
        ViewSnapshot {
            root: root.clone(),
            freshness: Freshness::Current,
            next: None,
            graph_relations: None,
            rich_graph: None,
        },
    )
    .with_certificate(certificate(&root));
    let encoded = serde_json::to_vec(&view).expect("encode view");
    assert!(ViewDto::decode_with_certificate(&encoded, Some(capability.clone())).is_ok());

    let mut forged: serde_json::Value = serde_json::from_slice(&encoded).expect("view value");
    let claims = forged["certificate"]["claims"]
        .as_array_mut()
        .expect("certificate claims");
    let coverage = claims
        .iter_mut()
        .find(|claim| claim["kind"] == "coverage")
        .expect("coverage claim");
    coverage["data"]["producer"] = serde_json::json!(encode_id(&[0x42; 32]));
    assert!(
        ViewDto::decode_with_certificate(
            &serde_json::to_vec(&forged).expect("encode forged view"),
            Some(capability.clone()),
        )
        .is_err()
    );
    let mut wrong_context: serde_json::Value =
        serde_json::from_slice(&encoded).expect("view value");
    let context_claim = wrong_context["certificate"]["claims"]
        .as_array_mut()
        .expect("certificate claims")
        .iter_mut()
        .find(|claim| claim["kind"] == "coverage")
        .expect("coverage claim");
    context_claim["data"]["context"] = serde_json::json!(encode_id(&[0x43; 32]));
    assert!(
        ViewDto::decode_with_certificate(
            &serde_json::to_vec(&wrong_context).expect("encode wrong context"),
            Some(capability.clone()),
        )
        .is_err()
    );
    let mut wrong_evidence: serde_json::Value =
        serde_json::from_slice(&encoded).expect("view value");
    let evidence_claim = wrong_evidence["certificate"]["claims"]
        .as_array_mut()
        .expect("certificate claims")
        .iter_mut()
        .find(|claim| claim["kind"] == "coverage")
        .expect("coverage claim");
    evidence_claim["data"]["evidence"] = serde_json::json!([0x44]);
    assert!(
        ViewDto::decode_with_certificate(
            &serde_json::to_vec(&wrong_evidence).expect("encode wrong evidence"),
            Some(capability.clone()),
        )
        .is_err()
    );
    assert!(ViewDto::decode_with_certificate(&encoded, None).is_err());
}

#[test]
fn authenticated_verifier_mints_only_from_the_exact_wire_observation() {
    let source_root = view_state_root(&[]);
    let basis = Basis::new(source_root, object_version(b"source"));
    let root = ViewRoot::empty_checked(
        view_key(b"view"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, source_root, 0),
        capability(basis.object),
    )
    .expect("complete root");
    let certificate = certificate(&root);
    let reply = ReplyDto::health(41, root.clone(), Cursor::for_view_root(&root))
        .with_certificate(certificate);
    let encoded = serde_json::to_vec(&reply).expect("encode health reply");

    let decoded = crate::decode_reply_body_with_verifier(&encoded, &TestCoverageVerifier)
        .expect("authenticated verifier should admit exact observation");
    assert_eq!(decoded.request_id, reply.request_id);
    assert!(crate::decode_reply_body(&encoded).is_err());

    let mut forged: serde_json::Value = serde_json::from_slice(&encoded).expect("reply value");
    let claims = forged["certificate"]["claims"]
        .as_array_mut()
        .expect("certificate claims");
    let coverage = claims
        .iter_mut()
        .find(|claim| claim["kind"] == "coverage")
        .expect("coverage claim");
    coverage["data"]["context"] = serde_json::json!(encode_id(&[0x55; 32]));
    assert!(
        crate::decode_reply_body_with_verifier(
            &serde_json::to_vec(&forged).expect("encode forged reply"),
            &TestCoverageVerifier,
        )
        .is_err()
    );
}

#[test]
fn readiness_round_trip_carries_no_view_rows() -> Result<(), String> {
    let source_root = view_state_root(&[]);
    let basis = Basis::new(source_root, object_version(b"source"));
    let root = ViewRoot::empty_checked(
        view_key(b"view"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, source_root, 0),
        capability(basis.object),
    )
    .expect("complete root");
    let cursor = Cursor::for_view_root(&root);
    let reply = ReplyDto::new(
        47,
        CommandReply::Readiness(crate::HealthReport::from_root(&root, cursor)),
    )
    .with_certificate(certificate(&root).with_claim(WireClaim::RootCommitment {
        schema: WireSchema::ViewRelation,
        id: encode_id(root.root().as_bytes()),
    }));
    let encoded = serde_json::to_vec(&reply).expect("encode readiness");
    let value: serde_json::Value = serde_json::from_slice(&encoded).expect("readiness json");
    assert!(value["reply"]["data"].get("rows").is_none());
    assert!(value["reply"]["data"].get("row_count").is_some());
    let capability_rows = value["reply"]["data"]["capabilities"]["rows"]
        .as_array()
        .expect("bounded capability inventory");
    assert_eq!(capability_rows.len(), 28);
    assert!(capability_rows.iter().all(|row| {
        row["lifecycle"]["state"] == "unavailable" && row["lifecycle"]["reason"] == "no_manifest"
    }));

    let decoded = crate::decode_reply_body_with_verifier(&encoded, &TestCoverageVerifier)
        .expect("admit readiness");
    assert_eq!(decoded, reply);
    let CommandReply::Readiness(report) = decoded.reply else {
        return Err("decoded readiness reply changed shape".to_owned());
    };
    assert_eq!(report.capabilities().as_slice().len(), 28);
    Ok(())
}

#[test]
#[allow(clippy::too_many_lines)]
fn certified_subscription_rejects_wrong_root_schema_producer_and_replay() {
    let source_root = view_state_root(&[]);
    let basis = Basis::new(source_root, object_version(b"source"));
    let capability = capability(basis.object);
    let root = ViewRoot::empty_checked(
        view_key(b"view"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, source_root, 0),
        capability.clone(),
    )
    .expect("complete root");
    let previous = Cursor::for_view(
        root.recipe(),
        root.version(),
        Frontier::new(
            root.frontier().branch,
            root.frontier().log,
            root.frontier().schema,
            root.root(),
            0,
        ),
    );
    let cursor = Cursor::for_view(
        root.recipe(),
        root.version(),
        Frontier::new(
            root.frontier().branch,
            root.frontier().log,
            root.frontier().schema,
            root.root(),
            1,
        ),
    );
    let view = ViewDto::new(
        0,
        ViewSnapshot {
            root: root.clone(),
            freshness: Freshness::Current,
            next: None,
            graph_relations: None,
            rich_graph: None,
        },
    )
    .with_certificate(certificate(&root));
    let subscription =
        SubscriptionDto::try_reset(previous, cursor, view, crate::CursorResetReason::Gap)
            .expect("certified reset");
    let encoded = serde_json::to_vec(&subscription).expect("encode reset");
    assert!(matches!(
        SubscriptionDto::decode(&encoded, previous, Some(capability.clone())),
        Ok(crate::CursorRead::Reset { .. })
    ));

    let mut wrong_root: serde_json::Value = serde_json::from_slice(&encoded).expect("value");
    wrong_root["cursor"]["root"] = serde_json::json!(encode_id(
        view_state_root(&[("forged".to_owned(), "root".to_owned())]).as_bytes()
    ));
    assert!(
        SubscriptionDto::decode(
            &serde_json::to_vec(&wrong_root).expect("wrong root"),
            previous,
            Some(capability.clone()),
        )
        .is_err()
    );

    let mut wrong_schema = serde_json::from_slice::<serde_json::Value>(&encoded).expect("value");
    wrong_schema["cursor"]["schema"] = serde_json::json!(u64::from(previous.schema()) + 1);
    assert!(
        SubscriptionDto::decode(
            &serde_json::to_vec(&wrong_schema).expect("wrong schema"),
            previous,
            Some(capability.clone()),
        )
        .is_err()
    );

    let mut wrong_producer = serde_json::from_slice::<serde_json::Value>(&encoded).expect("value");
    wrong_producer["root"]["certificate"]["claims"][0]["data"]["value"] =
        serde_json::json!([111, 116, 104, 101, 114]);
    assert!(
        SubscriptionDto::decode(
            &serde_json::to_vec(&wrong_producer).expect("wrong producer"),
            previous,
            Some(capability.clone()),
        )
        .is_err()
    );

    let mut replay = serde_json::from_slice::<serde_json::Value>(&encoded).expect("value");
    replay["cursor"]["sequence"] = serde_json::json!(previous.sequence());
    assert!(
        SubscriptionDto::decode(
            &serde_json::to_vec(&replay).expect("replay"),
            previous,
            Some(capability.clone()),
        )
        .is_err()
    );

    let health = ReplyDto::health(1, root.clone(), Cursor::for_view_root(&root))
        .with_certificate(certificate(&root));
    let projection = health
        .admit_complete_view_projection_with_capability(None, Some(capability.clone()))
        .expect("health projection");
    assert_eq!(projection.cursor(), Cursor::for_view_root(&root));

    let mut wrong_cursor: serde_json::Value = serde_json::to_value(&health).expect("health");
    wrong_cursor["health_cursor"]["sequence"] = serde_json::json!(1);
    assert!(
        crate::decode_reply_body(&serde_json::to_vec(&wrong_cursor).expect("wrong cursor"))
            .is_err()
    );
}

#[test]
fn shared_reply_admission_requires_one_exact_producer_projection() {
    let source_root = view_state_root(&[]);
    let basis = Basis::new(source_root, object_version(b"source"));
    let capability = capability(basis.object);
    let root = ViewRoot::empty_checked(
        view_key(b"view"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, source_root, 0),
        capability.clone(),
    )
    .expect("complete root");
    let request = CommandDto::new(31, Command::Health);

    assert!(
        admit_reply(
            &request,
            &ReplyDto::new(31, CommandReply::Health(root.clone()))
        )
        .is_err()
    );

    let certified = ReplyDto::health(31, root.clone(), Cursor::for_view_root(&root))
        .with_certificate(certificate(&root));
    assert!(admit_reply_with_capability(&request, &certified, Some(capability.clone())).is_ok());

    let other_base = root.clone();
    let prepared = other_base
        .prepare(
            ViewDelta::Upsert {
                row: Row::new(RowId::Symbol(symbol_key("other::Thing")), basis, "other"),
            },
            capability,
        )
        .expect("prepare other root");
    let (other, _) = other_base.commit(prepared).expect("other complete root");
    let forged = ReplyDto::health(31, root, Cursor::for_view_root(&other))
        .with_certificate(certificate(&other));
    assert!(admit_reply(&request, &forged).is_err());

    let replay = CommandDto::new(32, Command::Health);
    assert!(matches!(
        admit_reply(&replay, &certified),
        Err(crate::ReplyAdmissionError::RequestMismatch { .. })
    ));
}

#[test]
#[allow(clippy::too_many_lines)]
fn paged_reset_descriptor_keeps_root_certificate_constant_and_admits_final_rows() {
    let source_root = view_state_root(&[]);
    let basis = Basis::new(source_root, object_version(b"source"));
    let row = Row::new(RowId::Symbol(symbol_key("Thing")), basis, "Thing");
    let root = ViewRoot::new_checked(
        view_key(b"view"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, source_root, 0),
        vec![row.clone()],
        vec![crate::Coverage::Complete],
        capability(basis.object),
    )
    .expect("root");
    let descriptor = root.descriptor();
    let previous = Cursor::for_view_root(&root);
    let cursor = Cursor::for_view_root_at(&root, 1);
    let page = root.page(ViewPageCursor::first(&root), 1).expect("page");
    let mut certificate = WireCertificate::new()
        .with_claim(WireClaim::KeyBytes {
            schema: WireSchema::ViewRecipe,
            id: encode_id(root.recipe().as_bytes()),
            value: b"view".to_vec().into_boxed_slice(),
        })
        .with_claim(WireClaim::Version {
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
        })
        .with_claim(WireClaim::RootCommitment {
            schema: WireSchema::ViewRelation,
            id: encode_id(root.root().as_bytes()),
        })
        .with_claim(WireClaim::Root {
            schema: WireSchema::ViewRelation,
            id: encode_id(root.basis().root.as_bytes()),
            canonical: backend_version::canonical_empty::<crate::ViewRelation>()
                .as_bytes()
                .to_vec()
                .into_boxed_slice(),
        })
        .with_claim(WireClaim::Version {
            schema: WireSchema::Object,
            id: encode_id(root.basis().object.as_bytes()),
            value: b"source".to_vec().into_boxed_slice(),
        })
        .with_claim(WireClaim::Coverage {
            scope: encode_id(root.basis().object.as_bytes()),
            observed: encode_id(root.basis().object.as_bytes()),
            producer: encode_id(&capability(root.basis().object).producer_identity()),
            context: encode_id(&capability(root.basis().object).context()),
            evidence: capability(root.basis().object)
                .evidence()
                .to_vec()
                .into_boxed_slice(),
        })
        .with_claim(WireClaim::Key {
            schema: WireSchema::Branch,
            id: encode_id(root.basis().branch.as_bytes()),
            value: "main".to_owned(),
        })
        .with_claim(WireClaim::Key {
            schema: WireSchema::Log,
            id: encode_id(root.basis().log.as_bytes()),
            value: "library".to_owned(),
        })
        .with_claim(WireClaim::Key {
            schema: WireSchema::Symbol,
            id: encode_id(symbol_key("Thing").as_bytes()),
            value: "Thing".to_owned(),
        });
    certificate = certificate.with_claim(WireClaim::Cursor {
        recipe: encode_id(cursor.recipe().as_bytes()),
        version: encode_id(cursor.version().as_bytes()),
        branch: encode_id(cursor.branch().as_bytes()),
        log: encode_id(cursor.log().as_bytes()),
        schema: cursor.schema(),
        root: encode_id(cursor.root().as_bytes()),
        sequence: cursor.sequence(),
    });
    let page = SnapshotPageDto::try_new(
        previous,
        cursor,
        descriptor.clone(),
        page,
        crate::CursorResetReason::Pruned,
    )
    .expect("snapshot page")
    .with_certificate(certificate);
    let encoded = serde_json::to_vec(&page).expect("encode page");
    assert!(encoded.len() < 16 * 1024);
    let decoded =
        SnapshotPageDto::decode_against(&encoded, previous, &descriptor).expect("decode page");
    let restored = descriptor
        .clone()
        .admit_rows(decoded.page().rows().to_vec())
        .expect("admit final root");
    assert_eq!(restored.root(), root.root());
    assert_eq!(decoded.cursor(), cursor);

    let mut forged: serde_json::Value = serde_json::from_slice(&encoded).expect("value");
    forged["root"]["root"] = serde_json::json!(encode_id(
        view_state_root(&[("wrong".to_owned(), "root".to_owned())]).as_bytes()
    ));
    assert!(
        SnapshotPageDto::decode_against(
            &serde_json::to_vec(&forged).expect("forge"),
            previous,
            &descriptor,
        )
        .is_err()
    );
}

#[test]
fn shared_request_admission_keeps_text_bounds_out_of_process_clients() {
    let empty = CommandDto::new(
        40,
        Command::Resolve {
            text: String::new(),
        },
    );
    assert_eq!(admit_request(&empty), Err(RequestAdmissionError::EmptyText));

    let oversized = CommandDto::new(
        41,
        Command::Resolve {
            text: "x".repeat(crate::MAX_COMMAND_TEXT + 1),
        },
    );
    assert_eq!(
        admit_request(&oversized),
        Err(RequestAdmissionError::TextTooLarge)
    );

    let accepted = CommandDto::new(
        42,
        Command::Resolve {
            text: "x".repeat(crate::MAX_COMMAND_TEXT),
        },
    );
    assert!(admit_request(&accepted).is_ok());
}

#[test]
fn graph_query_page_wire_admission_enforces_credit_and_exact_progress() {
    let source_root = view_state_root(&[]);
    let basis = Basis::new(source_root, object_version(b"source"));
    let root = ViewRoot::empty_checked(
        view_key(b"view"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, source_root, 7),
        capability(basis.object),
    )
    .expect("root");
    let owner = Cursor::for_view_root(&root);
    let query = crate::GraphQueryRequest::new(
        "{ Declaration { coordinate @output } }",
        std::collections::BTreeMap::new(),
        root.root(),
        QueryLimit::new(1).expect("limit"),
    )
    .expect("query");
    let row = crate::GraphQueryRow::new(std::collections::BTreeMap::from([(
        "coordinate".to_owned(),
        crate::GraphValue::String("a".to_owned()),
    )]))
    .expect("row");
    let mut proof = certificate(&root).with_claim_once(WireClaim::KeyBytes {
        schema: WireSchema::ViewRecipe,
        id: encode_id(query.recipe().as_bytes()),
        value: query.recipe_preimage(),
    });
    proof = proof.with_claim_once(WireClaim::RootCommitment {
        schema: WireSchema::ViewRelation,
        id: encode_id(root.root().as_bytes()),
    });
    let request =
        CommandDto::new(1, Command::GraphQuery(query.clone())).with_certificate(proof.clone());
    let good = crate::GraphQueryPage {
        revision: root.root().into(),
        source: basis.object,
        rows: vec![row.clone()].into_boxed_slice(),
        terminal: crate::PageTerminal::More(query.next_continuation(owner, 1).expect("next")),
    };
    let decode_with_proof = |page, proof: &WireCertificate| {
        let dto =
            ReplyDto::new(1, CommandReply::GraphQueryPage(page)).with_certificate(proof.clone());
        let bytes = serde_json::to_vec(&dto).expect("actual reply wire");
        ReplyDto::decode_with_certificate(&bytes, root.capability())
            .expect("strict proof admission")
    };
    let decode = |page| decode_with_proof(page, &proof);
    assert!(admit_reply(&request, &decode(good.clone())).is_ok());
    let mut oversized = good.clone();
    oversized.rows = vec![row.clone(), row].into_boxed_slice();
    let mut empty = good.clone();
    empty.rows = Box::new([]);
    let mut skipped = good.clone();
    skipped.terminal =
        crate::PageTerminal::More(query.next_continuation(owner, 9).expect("skipped"));
    let mut cancelled = good.clone();
    cancelled.rows = Box::new([]);
    cancelled.terminal = crate::PageTerminal::Cancelled;
    for page in [oversized, empty, skipped, cancelled] {
        assert!(admit_reply(&request, &decode(page)).is_err());
    }
    let foreign_branch = crate::branch_key("foreign");
    let foreign_log = crate::log_key("foreign");
    let foreign_version = crate::canonical::view_version(b"foreign-stream");
    let foreign_proof = proof
        .clone()
        .with_claim(WireClaim::Key {
            schema: WireSchema::Branch,
            id: encode_id(foreign_branch.as_bytes()),
            value: "foreign".to_owned(),
        })
        .with_claim(WireClaim::Key {
            schema: WireSchema::Log,
            id: encode_id(foreign_log.as_bytes()),
            value: "foreign".to_owned(),
        })
        .with_claim(WireClaim::Version {
            schema: WireSchema::ViewVersion,
            id: encode_id(foreign_version.as_bytes()),
            value: b"foreign-stream".to_vec().into_boxed_slice(),
        });
    for (recipe, version, branch, log, schema, sequence) in [
        (
            query.recipe(),
            owner.version(),
            foreign_branch,
            owner.log(),
            owner.schema(),
            owner.sequence(),
        ),
        (
            query.recipe(),
            owner.version(),
            owner.branch(),
            foreign_log,
            owner.schema(),
            owner.sequence(),
        ),
        (
            query.recipe(),
            foreign_version,
            owner.branch(),
            owner.log(),
            owner.schema(),
            owner.sequence(),
        ),
        (
            query.recipe(),
            owner.version(),
            owner.branch(),
            owner.log(),
            owner.schema() + 1,
            owner.sequence(),
        ),
        (
            query.recipe(),
            owner.version(),
            owner.branch(),
            owner.log(),
            owner.schema(),
            owner.sequence() + 1,
        ),
        (
            owner.recipe(),
            owner.version(),
            owner.branch(),
            owner.log(),
            owner.schema(),
            owner.sequence(),
        ),
    ] {
        let foreign = Cursor::for_view(
            recipe,
            version,
            Frontier::new(branch, log, schema, owner.root(), sequence),
        )
        .with_query_offset(1);
        let mut page = good.clone();
        page.terminal = crate::PageTerminal::More(crate::PageContinuation::from_cursor(foreign));
        assert!(
            admit_reply(&request, &decode_with_proof(page, &foreign_proof)).is_err(),
            "canonical foreign cursor must not become an issued token"
        );
    }
    let two = crate::GraphQueryRequest::bind(
        query.input().clone(),
        root.root(),
        QueryLimit::new(2).expect("two"),
    );
    let two_proof = proof.clone().with_claim_once(WireClaim::KeyBytes {
        schema: WireSchema::ViewRecipe,
        id: encode_id(two.recipe().as_bytes()),
        value: two.recipe_preimage(),
    });
    let mut short = good.clone();
    short.terminal = crate::PageTerminal::More(two.next_continuation(owner, 1).expect("short"));
    let two_request =
        CommandDto::new(1, Command::GraphQuery(two)).with_certificate(two_proof.clone());
    assert!(
        admit_reply(&two_request, &decode_with_proof(short, &two_proof)).is_err(),
        "More requires the full credit"
    );
    let resumed = CommandDto::new(
        1,
        Command::GraphQuery(
            query
                .clone()
                .with_continuation(query.next_continuation(owner, 1).expect("start")),
        ),
    )
    .with_certificate(proof.clone());
    assert!(
        admit_reply(&resumed, &decode(good)).is_err(),
        "repeated offset"
    );
}

#[path = "index_ticket_reply_tests.rs"]
mod index_ticket_reply;

#[test]
fn native_worker_refusal_wire_cutover_checks_version_before_payload_variants()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::interface::TypeScriptAuthorityFailureKind as F;
    assert_eq!(
        DTO_VERSION, 39,
        "current matched cohort retains catalog windows, admission details and worker refusal"
    );
    let kind = F::NativeCheckerWorkerPoolUnavailable;
    let encoded_kind = serde_json::to_vec(&kind)?;
    assert_eq!(encoded_kind, b"\"native_checker_worker_pool_unavailable\"");
    assert_eq!(serde_json::from_slice::<F>(&encoded_kind)?, kind);
    // Exact maintained preflight algorithm parameterized by the supported
    // protocol. This is a codec law, not an old-binary process replay.
    let current = serde_json::to_vec(&serde_json::json!({
        "version": DTO_VERSION,
        "reply": {"typescript_failure": kind, "unknown_future_shape": true}
    }))?;
    let old_peer = super::codec::check_version_header_against(&current, "reply", 27)
        .expect_err("DTO27 peer must refuse the current DTO before decoding its failure variant");
    assert!(old_peer.contains(&format!("reply DTO version {DTO_VERSION}")));
    assert!(old_peer.contains("this build supports 27"));
    assert!(!old_peer.contains("unknown variant"));
    let mut previous: serde_json::Value = serde_json::from_slice(&current)?;
    previous["version"] = serde_json::json!(27);
    previous["reply"]["typescript_failure"] = serde_json::json!("unknown_future_variant");
    let bytes = serde_json::to_vec(&previous)?;
    let error = crate::wire::decode_reply_body(&bytes)
        .err()
        .ok_or_else(|| std::io::Error::other("strict old live envelope must be refused"))?;
    assert!(error.contains("reply DTO version 27"));
    assert!(!error.contains("unknown variant"));
    Ok(())
}

#[test]
fn semantic_source_current_live_dto_refuses_dto28_before_source_or_epoch_payload() {
    let packet: serde_json::Value = serde_json::from_str(include_str!(
        "../../present/fixtures/semantic-versions-semver-public.json"
    ))
    .expect("unchanged actual source capture");
    let record: crate::SemanticVersionRecord =
        serde_json::from_value(packet["surface"]["data"][0].clone()).expect("exact typed record");
    let root = record
        .selected_source_frontier
        .as_ref()
        .expect("captured frontier")
        .source_relation_root;
    let reference = crate::SemanticSourceReference::capture(&record, root, [34; 32], &[7; 32])
        .expect("selector only; not native authority");
    let command = crate::CommandDto::new(
        1,
        crate::Command::Surface(crate::SurfaceCommand::SemanticSource {
            request: crate::SemanticSourceRequest {
                reference,
                range: Default::default(),
            },
        }),
    );
    let bytes = serde_json::to_vec(&command).expect("current exact live DTO");
    let error = super::codec::check_version_header_against(&bytes, "command", 28)
        .expect_err("old peer must refuse before new payload variant");
    assert!(error.contains(&format!("command DTO version {DTO_VERSION}")));
    assert!(error.contains("this build supports 28"));
    let mut downgraded: serde_json::Value = serde_json::from_slice(&bytes).expect("wire envelope");
    downgraded["version"] = serde_json::json!(28);
    downgraded["command"] = serde_json::json!({"operation":"unknown_future_source"});
    let error = crate::decode_command_body(&serde_json::to_vec(&downgraded).expect("old envelope"))
        .expect_err("current peer refuses old version first");
    assert!(error.contains("command DTO version 28"));
    assert!(!error.contains("unknown variant"));
}

#[path = "typescript_admission_cutover_tests.rs"]
mod typescript_admission_cutover;

#[test]
fn windowed_catalog_admission_checks_actual_delivery_offset_and_predecessor_span() {
    let (library, mut ids) = paged_query_fixture(0);
    ids.sort();
    let issued = QueryLimit::new(4).expect("issued");
    let window =
        crate::QueryWindow::new(issued, QueryLimit::new(2).expect("delivery")).expect("window");
    let query = NameQuery::new("Thing", library.revision_root(), issued)
        .with_window(window)
        .expect("window");
    let names = library.names(&query).expect("native names");
    let symbol = symbol_key("pkg::Thing0");
    let address = crate::SymbolAddress::canonical(symbol);
    let page = crate::PageRequest::new(library.revision_root(), issued)
        .with_window(window)
        .expect("window");
    let graph = library
        .graph_page_from_ids(symbol, address, page, &ids)
        .expect("native selected graph");
    for (command, snapshot) in [
        (Command::Name(query.clone()), names),
        (
            Command::GraphPage {
                symbol: address,
                page,
            },
            graph.snapshot,
        ),
    ] {
        let reply = |snapshot: ViewSnapshot| match &command {
            Command::Name(_) => CommandReply::Names(snapshot),
            _ => CommandReply::ProjectionPage(crate::ProjectionPage {
                outline_order: None,
                terminal: snapshot
                    .next
                    .map_or(crate::PageTerminal::Complete, |cursor| {
                        crate::PageTerminal::More(crate::PageContinuation::from_cursor(cursor))
                    }),
                snapshot,
            }),
        };
        let request = CommandDto::new(1, command.clone());
        assert!(admit_reply(&request, &ReplyDto::new(1, reply(snapshot.clone()))).is_ok());
        let next = snapshot.next.expect("native More");
        let bad_positions = [
            crate::QueryPosition::legacy(2),
            crate::QueryPosition::windowed(3, window).expect("wrong offset"),
            crate::QueryPosition::windowed(
                2,
                crate::QueryWindow::new(issued, QueryLimit::new(1).expect("wrong span"))
                    .expect("window"),
            )
            .expect("wrong span"),
            crate::QueryPosition::windowed(
                2,
                crate::QueryWindow::new(
                    QueryLimit::new(3).expect("wrong credit"),
                    QueryLimit::new(2).expect("span"),
                )
                .expect("window"),
            )
            .expect("wrong credit"),
        ];
        for position in bad_positions {
            let mut bad = snapshot.clone();
            bad.next = Some(next.with_query_position(position));
            assert!(
                admit_reply(&request, &ReplyDto::new(1, reply(bad))).is_err(),
                "forged cursor advance/span/credit"
            );
        }
    }
}

#[test]
fn catalog_window_current_cohort_refuses_dto29_and_keeps_owner_journal_grammar() {
    assert_eq!(DTO_VERSION, 39);
    let issued = QueryLimit::new(4).expect("issued");
    let fixed = CommandDto::new(
        1,
        Command::Name(NameQuery::new("Thing", view_state_root(&[]), issued)),
    );
    let legacy = serde_json::to_value(&fixed).expect("legacy shape");
    assert!(legacy["command"]["data"].get("window").is_none());
    let window =
        crate::QueryWindow::new(issued, QueryLimit::new(2).expect("delivery")).expect("window");
    let command = CommandDto::new(
        1,
        Command::Name(
            NameQuery::new("Thing", view_state_root(&[]), issued)
                .with_window(window)
                .expect("window"),
        ),
    );
    let bytes = serde_json::to_vec(&command).expect("new command");
    let old_peer = super::codec::check_version_header_against(&bytes, "command", 29)
        .expect_err("old peer rejects before its deny-unknown-fields decoder");
    assert!(old_peer.contains(&format!("command DTO version {DTO_VERSION}")));
    let mut old: serde_json::Value = serde_json::from_slice(&bytes).expect("envelope");
    old["version"] = serde_json::json!(29);
    let error = crate::decode_command_body(&serde_json::to_vec(&old).expect("old envelope"))
        .expect_err("old version rejected before window shape");
    assert!(error.contains("command DTO version 29"));
    assert!(!error.contains("unknown field"));
    let grammar = crate::JournalViewGrammarV3::from_checked_container(3).expect("existing grammar");
    assert!(grammar.check_envelope_version(29).is_ok());
    assert!(grammar.check_envelope_version(30).is_ok());
    assert!(grammar.check_envelope_version(31).is_ok());
    assert!(grammar.check_envelope_version(32).is_ok());
    assert!(grammar.check_envelope_version(33).is_ok());
    assert!(grammar.check_envelope_version(DTO_VERSION).is_ok());
    assert!(grammar.check_envelope_version(DTO_VERSION + 1).is_err());
}

#[test]
fn legacy_wire_empty_initial_owner_cursor_admission_is_preserved() {
    let (library, _) = paged_query_fixture(0);
    let query = NameQuery::new(
        "absent",
        library.revision_root(),
        QueryLimit::new(2).expect("credit"),
    );
    let empty = library.names(&query).expect("actual empty initial page");
    let owner_position = Cursor::for_view(
        empty.root.recipe(),
        library.view().version(),
        library.cursor().frontier(),
    );
    assert_eq!(owner_position.query_offset(), 0);
    let command = CommandDto::new(1, Command::Name(query.with_cursor(owner_position)));
    assert!(
        admit_reply(&command, &ReplyDto::new(1, CommandReply::Names(empty))).is_ok(),
        "shared wire admission historically allows an empty initial owner position"
    );
}

#[test]
fn borrowed_catalog_wire_is_byte_identical_to_the_existing_owned_projection() {
    for case in 0..6 {
        let (base, ids) = paged_query_fixture(0);
        let mut rows = base.view().rows().to_vec();
        for (i, row) in rows.iter_mut().enumerate() {
            row.document = vec![
                Fragment::Text("quotes\"\\\nאב🙂".repeat(20)),
                Fragment::Code("fn f() {\n}\t".to_owned()),
                Fragment::Link {
                    label: "other\"".to_owned(),
                    target: symbol_key("pkg::Thing1"),
                },
                Fragment::Break,
            ]
            .into_boxed_slice();
            row.signature = (i != 0).then(|| "fn f(λ: &str)".to_owned());
            row.kind = (i != 1).then_some(crate::DeclarationKind::Function);
            row.parent = (i == 2).then_some(symbol_key("pkg::Thing1"));
            row.source = match case {
                0 | 5 => crate::SourceAvailability::Captured(
                    crate::SourceLocation::new("src/a\".rs", 2).expect("location"),
                ),
                1 => crate::SourceAvailability::NotCaptured,
                2 => crate::SourceAvailability::NotHydrated,
                3 => crate::SourceAvailability::Unconfigured,
                _ => crate::SourceAvailability::stale_file("src/old.rs").expect("stale file"),
            };
            row.excerpt = match case {
                0 => crate::SourceExcerpt::captured(
                    "fn f() { λ }",
                    crate::SourceExcerptExtent::Complete,
                )
                .expect("complete excerpt"),
                1 => crate::SourceExcerpt::NotCaptured,
                2 => crate::SourceExcerpt::NotHydrated,
                3 => crate::SourceExcerpt::Unconfigured,
                _ => {
                    crate::SourceExcerpt::captured("fn f(λ", crate::SourceExcerptExtent::Truncated)
                        .expect("truncated excerpt")
                }
            };
            row.facts = match i {
                0 => crate::DeclarationFacts::UNOBSERVED,
                1 => crate::DeclarationFacts {
                    deprecation: crate::Fact::Absent,
                    obligation: crate::Fact::Absent,
                },
                _ => crate::DeclarationFacts {
                    deprecation: crate::Fact::Present(crate::Deprecation::new(
                        Some("1.0"),
                        Some("use \"other\"\\λ"),
                    )),
                    obligation: crate::Fact::Present(crate::Obligation::Required),
                },
            };
        }
        let root = ViewRoot::new_checked(
            base.view().recipe(),
            base.view().basis(),
            base.view().frontier(),
            rows,
            base.view().coverage().to_vec(),
            base.view().capability().expect("owned fixture scope"),
        )
        .expect("real catalog root");
        let library =
            crate::Library::from_view(root.clone(), Cursor::for_view_root(&root)).expect("catalog");
        let limit = QueryLimit::new(4).expect("issued");
        let window =
            crate::QueryWindow::new(limit, QueryLimit::new(2).expect("delivery")).expect("window");
        let names = library
            .names(
                &NameQuery::new("Thing", library.revision_root(), limit)
                    .with_window(window)
                    .expect("names window"),
            )
            .expect("actual names");
        let search = library
            .search(
                &Query::new("Thing", library.revision_root(), limit)
                    .with_window(window)
                    .expect("search window"),
            )
            .expect("actual search");
        let mut sorted = ids.clone();
        sorted.sort();
        let graph = library
            .graph_page_from_ids(
                symbol_key("pkg::Thing0"),
                crate::SymbolAddress::canonical(symbol_key("pkg::Thing0")),
                crate::PageRequest::new(library.revision_root(), limit)
                    .with_window(window)
                    .expect("graph window"),
                &sorted,
            )
            .expect("actual selected graph");
        let names_tail = library
            .names(
                &NameQuery::new("Thing", library.revision_root(), limit)
                    .with_window(window)
                    .expect("names window")
                    .with_cursor(names.next.expect("names prefix")),
            )
            .expect("actual names tail");
        let search_tail = library
            .search(
                &Query::new("Thing", library.revision_root(), limit)
                    .with_window(window)
                    .expect("search window")
                    .with_cursor(search.next.expect("search prefix")),
            )
            .expect("actual search tail");
        let graph_tail = library
            .graph_page_from_ids(
                symbol_key("pkg::Thing0"),
                crate::SymbolAddress::canonical(symbol_key("pkg::Thing0")),
                crate::PageRequest::new(library.revision_root(), limit)
                    .with_window(window)
                    .expect("graph window")
                    .with_continuation(crate::PageContinuation::from_cursor(
                        graph.snapshot.next.expect("graph prefix"),
                    )),
                &sorted,
            )
            .expect("actual graph tail");
        for body in [
            CommandReply::Names(names),
            CommandReply::Search(search),
            CommandReply::ProjectionPage(graph),
            CommandReply::Names(names_tail),
            CommandReply::Search(search_tail),
            CommandReply::ProjectionPage(graph_tail),
        ] {
            // This comparison tests transport spelling only. Empty certificate
            // carries no producer authority, and no index/owner proof is claimed.
            let mut dto = ReplyDto::new(17, body).with_certificate(WireCertificate::new());
            if matches!(&dto.reply, CommandReply::Search(_)) {
                dto = dto.with_semantic_search_status(crate::SemanticSearchStatus::Available);
            }
            assert_catalog_bytes_match_owned(&dto);
        }
    }
}

fn assert_catalog_bytes_match_owned(dto: &ReplyDto) {
    let owned = super::reply::ReplyEnvelope {
        version: DTO_VERSION,
        request_id: dto.request_id,
        reply: super::reply::reply_to_wire(&dto.reply).expect("maintained owned projection"),
        certificate: dto.certificate().cloned(),
        health_cursor: None,
        semantic_search: dto.semantic_search_status(),
    };
    let expected = serde_json::to_vec(&owned).expect("existing complete bytes");
    assert_eq!(
        serde_json::to_vec(dto).expect("borrowed serializer"),
        expected
    );
    assert_eq!(
        crate::encode_reply_body_bounded(dto).expect("bounded native body"),
        expected
    );
    assert_eq!(
        super::bounded_json::bounded_json(dto, expected.len()).expect("exact bound"),
        expected
    );
    assert!(matches!(
        super::bounded_json::bounded_json(dto, expected.len() - 1),
        Err(crate::BoundedJsonError::LimitExceeded { .. })
    ));
}

#[test]
fn borrowed_catalog_wire_keeps_real_producer_certificate_and_graph_sidecars() {
    let source = object_version(b"library-source-v1");
    let base = crate::Library::with_coverage(capability(source)).expect("covered built-in source");
    let basis = base.view().basis();
    let ids =
        ["pkg::Thing0", "pkg::Thing1", "pkg::Thing2"].map(|name| RowId::Symbol(symbol_key(name)));
    let rows = ids
        .iter()
        .enumerate()
        .map(|(i, id)| Row::new(*id, basis, format!("Thing{i}\"λ")))
        .collect();
    let root = ViewRoot::new_checked(
        base.view().recipe(),
        basis,
        base.view().frontier(),
        rows,
        base.view().coverage().to_vec(),
        base.view().capability().expect("built-in coverage"),
    )
    .expect("checked populated built-in source");
    let library = crate::Library::from_view(root.clone(), Cursor::for_view_root(&root))
        .expect("built-in producer");
    // Obtain real claims through the production certificate constructor. This
    // transport-equivalence test does not claim these revision claims alone
    // independently certify the selected catalog projection or its sidecars.
    let revision = library.execute_dto(CommandDto::new(1, Command::Revision));
    assert!(matches!(&revision.reply, CommandReply::Revision(_)));
    let proof = revision
        .certificate()
        .expect("production revision certificate");
    assert!(!proof.claims.is_empty());
    let limit = QueryLimit::new(4).expect("complete three-row selection");
    let mut sorted = ids;
    sorted.sort();
    let graph = library
        .graph_page_from_ids(
            symbol_key("pkg::Thing0"),
            crate::SymbolAddress::canonical(symbol_key("pkg::Thing0")),
            crate::PageRequest::new(library.revision_root(), limit),
            &sorted,
        )
        .expect("actual graph selection");
    let names = library
        .names(&NameQuery::new("Thing", library.revision_root(), limit))
        .expect("actual names selection");
    let search = library
        .search(&Query::new("Thing", library.revision_root(), limit))
        .expect("actual search selection");
    for mut body in [
        CommandReply::Names(names),
        CommandReply::Search(search),
        CommandReply::ProjectionPage(graph),
    ] {
        let snapshot = match &mut body {
            CommandReply::Names(snapshot) | CommandReply::Search(snapshot) => snapshot,
            CommandReply::ProjectionPage(page) => &mut page.snapshot,
            _ => unreachable!("three catalog families"),
        };
        assert_eq!(snapshot.root.rows().len(), 3);
        let relations = vec![crate::GraphRelation::new(
            ids[0],
            ids[1],
            crate::SemanticLinkKind::Calls,
        ).with_confidence(crate::SemanticConfidence::Syntactic)]
        .into_boxed_slice();
        let revision = crate::RichGraphRevision::new(
            snapshot.root.root().to_bytes(),
            crate::SemanticGenerationId::new([7; 32]),
            [8; 32],
        );
        let rich =
            crate::RichGraphSnapshot::from_view(&snapshot.root, ids[0], &relations, revision)
                .expect("checked nonempty graph sidecar");
        assert_eq!(rich.nodes.len(), 3);
        assert_eq!(rich.edges.len(), 1);
        assert_eq!(rich.edges[0].provenance.confidence, Some(crate::SemanticConfidence::Syntactic));
        snapshot.graph_relations = Some(relations);
        snapshot.rich_graph = Some(rich);
        let dto = ReplyDto::new(18, body).with_certificate(proof.clone());
        assert_catalog_bytes_match_owned(&dto);
        let value = serde_json::to_value(&dto).expect("catalog sidecar shape");
        let snapshot = if matches!(&dto.reply, CommandReply::ProjectionPage(_)) {
            &value["reply"]["data"]["snapshot"]
        } else {
            &value["reply"]["data"]
        };
        assert_eq!(
            snapshot["graph_relations"]["relations"]
                .as_array()
                .expect("typed edges")
                .len(),
            1
        );
        assert_eq!(snapshot["graph_relations"]["schema"], 2);
        assert_eq!(snapshot["graph_relations"]["relations"][0]["confidence"], "syntactic");
        assert_eq!(snapshot["rich_graph"]["edges"][0]["provenance"]["confidence"], "syntactic");
        assert_eq!(
            snapshot["rich_graph"]["nodes"]
                .as_array()
                .expect("rich nodes")
                .len(),
            3
        );
        assert_eq!(
            snapshot["rich_graph"]["edges"]
                .as_array()
                .expect("rich edges")
                .len(),
            1
        );
        assert_eq!(
            value["certificate"]["claims"]
                .as_array()
                .expect("production claims")
                .len(),
            proof.claims.len()
        );
    }
}

#[test]
fn settled_gap_current_cohort_rejects_old30_before_payload_and_requires_a_matched_cohort() {
    assert_eq!(DTO_VERSION, 39);
    // Use the real strict coverage projection, rather than a hand-written field.
    let coverage = super::reply_coverage::coverage_to_wire(crate::Coverage::Gapped {
        lane: crate::Lane::Semantic,
    });
    let projected = serde_json::to_value(coverage).expect("new strict variant");
    let current = serde_json::to_vec(&serde_json::json!({
        "version": DTO_VERSION,
        "reply": {"coverage": projected, "unknown_future_field": true}
    }))
    .expect("cohort envelope");
    let old_peer = super::codec::check_version_header_against(&current, "reply", 30)
        .expect_err("old peer stops before decoding a gapped variant");
    assert!(old_peer.contains("reply DTO version 39"));
    assert!(old_peer.contains("same build"));
    assert!(!old_peer.contains("unknown variant"));
    // A mutation under the previous cohort is refused before its nested shape.
    let old_mutation = br#"{"version":30,"request_id":1,"command":{"index_operation_start":{"unknown_future_field":true}}}"#;
    let error = crate::decode_command_body(old_mutation).expect_err("old live mutation refused");
    assert!(error.contains("command DTO version 30"));
    assert!(error.contains("this build supports 39"));
    assert!(!error.contains("unknown field"));
    let grammar = crate::JournalViewGrammarV3::from_checked_container(3).expect("same container");
    for version in 21..=37 {
        grammar
            .check_envelope_version(version)
            .expect("explicit maintained history");
    }
    assert!(grammar.check_envelope_version(33).is_ok());
    assert!(grammar.check_envelope_version(DTO_VERSION).is_ok());
    assert!(grammar.check_envelope_version(DTO_VERSION + 1).is_err());
}

#[test]
fn settled_gap_snapshot_rechecks_its_actual_root_and_certificate_in_the_same_journal_container() {
    // The maintained certificate fixture owns these exact preimages.
    let basis = Basis::new(view_state_root(&[]), object_version(b"source"));
    let root = ViewRoot::new_checked(
        view_key(b"view"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
        Vec::new(),
        vec![
            crate::Coverage::Complete,
            crate::Coverage::Gapped {
                lane: crate::Lane::Semantic,
            },
        ],
        capability(basis.object),
    )
    .expect("actual checked gapped projection");
    let dto = ViewDto::new(
        1,
        ViewSnapshot {
            root: root.clone(),
            freshness: Freshness::Current,
            next: None,
            graph_relations: None,
            rich_graph: None,
        },
    )
    .with_certificate(certificate(&root));
    let bytes = serde_json::to_vec(&dto).expect("current gapped snapshot");
    let live = ViewDto::decode_with_certificate(&bytes, Some(capability(basis.object)))
        .expect("current live certificate admission");
    assert_eq!(live.snapshot.root, root);
    let grammar = crate::JournalViewGrammarV3::from_checked_container(3)
        .expect("unchanged journal container");
    assert_eq!(
        grammar
            .decode_snapshot(&bytes, capability(basis.object))
            .expect("strict persisted proof")
            .snapshot
            .root,
        root
    );
    // DTO32 changes live refusals only. This synthetic historical envelope
    // retains the exact Gapped root/certificate; it is not an old-build capture.
    for version in [31, 35] {
        let mut historical: serde_json::Value = serde_json::from_slice(&bytes).expect("wire");
        historical["version"] = serde_json::json!(version);
        let historical = serde_json::to_vec(&historical).expect("historical envelope");
        assert!(
            ViewDto::decode_with_certificate(&historical, Some(capability(basis.object))).is_err()
        );
        assert_eq!(
            grammar
                .decode_snapshot(&historical, capability(basis.object))
                .expect("historical snapshot keeps strict proof admission")
                .snapshot
                .root,
            root
        );
    }
    let mut forged: serde_json::Value = serde_json::from_slice(&bytes).expect("wire");
    forged["snapshot"]["root"]["coverage"] = serde_json::json!([]);
    assert!(
        grammar
            .decode_snapshot(
                &serde_json::to_vec(&forged).expect("tampered wire"),
                capability(basis.object)
            )
            .is_err(),
        "coverage cannot change without its canonical root certificate"
    );
}

#[test]
fn graph_confidence_versions_round_trip_without_promoting_legacy_or_conflicting_evidence()
-> Result<(), Box<dyn std::error::Error>> {
    let basis = Basis::new(view_state_root(&[]), object_version(b"source"));
    let labels = ["pkg::from", "pkg::to"];
    let ids = labels.map(|label| RowId::Symbol(symbol_key(label)));
    let root = ViewRoot::new_checked(
        view_key(b"view"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
        labels
            .iter()
            .zip(ids)
            .map(|(label, id)| Row::new(id, basis, *label))
            .collect(),
        vec![crate::Coverage::Complete],
        capability(basis.object),
    )
    .expect("checked source rows");
    let mut proof = certificate(&root);
    for label in labels {
        proof = proof.with_claim(WireClaim::Key {
            schema: WireSchema::Symbol,
            id: encode_id(symbol_key(label).as_bytes()),
            value: label.to_owned(),
        });
    }
    let relation = crate::GraphRelation::new(ids[0], ids[1], crate::SemanticLinkKind::Calls)
        .with_confidence(crate::SemanticConfidence::Syntactic);
    let revision = crate::RichGraphRevision::new(
        root.root().to_bytes(),
        crate::SemanticGenerationId::new([7; 32]),
        [8; 32],
    );
    let rich = crate::RichGraphSnapshot::from_view(&root, ids[0], &[relation], revision)
        .expect("observed confidence graph");
    let snapshot = ViewSnapshot {
        root: root.clone(),
        freshness: Freshness::Current,
        next: None,
        graph_relations: Some(vec![relation].into_boxed_slice()),
        rich_graph: Some(rich),
    };
    let current = serde_json::to_value(super::reply::snapshot_to_wire(&snapshot))?;
    let decode = |value: serde_json::Value| -> Result<ViewSnapshot, String> {
        let wire = serde_json::from_value(value).map_err(|error| error.to_string())?;
        super::reply::snapshot_from_wire(wire, &proof, Some(capability(basis.object)))
    };
    let actual = decode(current.clone()).map_err(std::io::Error::other)?;
    assert_eq!(actual.root.root(), root.root());
    assert_eq!(
        actual
            .graph_relations
            .as_ref()
            .ok_or_else(|| std::io::Error::other("graph relations omitted"))?[0]
            .confidence,
        Some(crate::SemanticConfidence::Syntactic)
    );
    assert_eq!(
        actual
            .rich_graph
            .as_ref()
            .ok_or_else(|| std::io::Error::other("rich graph omitted"))?
            .edges[0]
            .provenance
            .confidence,
        Some(crate::SemanticConfidence::Syntactic)
    );

    let mut legacy = current.clone();
    legacy["graph_relations"]["schema"] = 1.into();
    legacy["graph_relations"]["relations"][0]
        .as_object_mut()
        .ok_or_else(|| std::io::Error::other("graph relation must be a JSON object"))?
        .remove("confidence");
    legacy["rich_graph"]["edges"][0]["provenance"]["confidence"] = "compiler".into();
    let decoded_legacy = decode(legacy.clone()).map_err(std::io::Error::other)?;
    assert_eq!(
        decoded_legacy
            .graph_relations
            .as_ref()
            .ok_or_else(|| std::io::Error::other("legacy graph relations omitted"))?[0]
            .confidence,
        None
    );
    let legacy_rich = decoded_legacy
        .rich_graph
        .as_ref()
        .ok_or_else(|| std::io::Error::other("legacy rich graph omitted"))?;
    assert_eq!(legacy_rich.edges[0].provenance.confidence, None);
    assert_eq!(
        legacy_rich.edges[0].id,
        actual
            .rich_graph
            .as_ref()
            .ok_or_else(|| std::io::Error::other("current rich graph omitted"))?
            .edges[0]
            .id
    );
    assert_eq!(
        legacy_rich.layout,
        actual
            .rich_graph
            .as_ref()
            .ok_or_else(|| std::io::Error::other("current rich graph omitted"))?
            .layout
    );
    assert_eq!(legacy_rich.revision, revision);
    assert_eq!(decoded_legacy.root.root(), root.root());
    let mut legacy_without_rich = legacy.clone();
    legacy_without_rich["rich_graph"] = serde_json::Value::Null;
    let legacy_without_rich = decode(legacy_without_rich).map_err(std::io::Error::other)?;
    assert_eq!(
        legacy_without_rich
            .graph_relations
            .as_ref()
            .ok_or_else(|| std::io::Error::other("legacy graph relations omitted"))?[0]
            .confidence,
        None
    );

    let mut unknown = current.clone();
    unknown["graph_relations"]["relations"][0]["confidence"] = serde_json::Value::Null;
    unknown["rich_graph"] = serde_json::Value::Null;
    let unknown = decode(unknown).map_err(std::io::Error::other)?;
    assert_eq!(
        unknown
            .graph_relations
            .as_ref()
            .ok_or_else(|| std::io::Error::other("unknown graph relations omitted"))?[0]
            .confidence,
        None
    );
    let mut bad_schema = current.clone();
    bad_schema["graph_relations"]["schema"] = 3.into();
    assert!(decode(bad_schema).is_err());
    let mut legacy_claim = legacy;
    legacy_claim["graph_relations"]["relations"][0]["confidence"] = "compiler".into();
    assert!(decode(legacy_claim).is_err());
    let mut bad_confidence = current.clone();
    bad_confidence["graph_relations"]["relations"][0]["confidence"] =
        "stronger-than-compiler".into();
    assert!(decode(bad_confidence).is_err());
    let mut extra = current.clone();
    extra["graph_relations"]["relations"][0]["future_authority"] = true.into();
    assert!(decode(extra).is_err());
    let mut duplicate = current.clone();
    let mut conflict = duplicate["graph_relations"]["relations"][0].clone();
    conflict["confidence"] = "compiler".into();
    duplicate["graph_relations"]["relations"]
        .as_array_mut()
        .ok_or_else(|| std::io::Error::other("graph relations must be a JSON array"))?
        .push(conflict);
    assert!(decode(duplicate).is_err());
    let mut disagreement = current;
    disagreement["rich_graph"]["edges"][0]["provenance"]["confidence"] = "compiler".into();
    let error = decode(disagreement)
        .err()
        .ok_or_else(|| std::io::Error::other("conflicting confidence must be rejected"))?;
    assert!(error.contains("confidence"));
    Ok(())
}

#[test]
fn direct_member_wire_preserves_selected_owner_source_credit_and_recipe() {
    let (library, ids) = paged_query_fixture(0);
    let RowId::Symbol(owner) = ids[0] else {
        panic!("symbol fixture");
    };
    let source = Basis {
        root: library.revision_root(),
        ..library.view().basis()
    };
    let query = DocumentQuery::selected(owner, source.root).with_source_basis(source);
    let credit = QueryLimit::new(200).expect("credit");
    let command = CommandDto::new(
        81,
        Command::MembersPage {
            owner: query,
            page: crate::PageRequest::new(source.root, credit),
        },
    );
    let bytes = serde_json::to_vec(&command).expect("wire");
    assert_eq!(
        CommandDto::decode_against(&bytes, &command).expect("exact command"),
        command
    );
    let page = library
        .members_page(query, crate::PageRequest::new(source.root, credit))
        .expect("empty member set still proves owner");
    assert_eq!(page.snapshot.root.row_count(), 1);
    let reply = ReplyDto::new(81, CommandReply::ProjectionPage(page.clone()));
    assert_eq!(admit_reply(&command, &reply), Ok(()));
    let RowId::Symbol(other) = ids[1] else {
        panic!("symbol fixture");
    };
    let wrong_owner = CommandDto::new(
        81,
        Command::MembersPage {
            owner: DocumentQuery::selected(other, source.root).with_source_basis(source),
            page: crate::PageRequest::new(source.root, credit),
        },
    );
    assert!(admit_reply(&wrong_owner, &reply).is_err());
    let wrong_source = Basis {
        object: object_version(b"other source"),
        ..source
    };
    let wrong_source = CommandDto::new(
        81,
        Command::MembersPage {
            owner: query.with_source_basis(wrong_source),
            page: crate::PageRequest::new(source.root, credit),
        },
    );
    assert!(admit_reply(&wrong_source, &reply).is_err());
    let wrong_credit = CommandDto::new(
        81,
        Command::MembersPage {
            owner: query,
            page: crate::PageRequest::new(source.root, QueryLimit::new(199).expect("credit")),
        },
    );
    assert!(admit_reply(&wrong_credit, &reply).is_err());
    let outline = CommandDto::new(
        81,
        Command::OutlinePage {
            package: crate::package_key("pkg"),
            page: crate::PageRequest::new(source.root, credit),
        },
    );
    assert!(admit_reply(&outline, &reply).is_err());
}

#[test]
fn published_name_stage_admission_cannot_claim_full_search_or_vector_success() {
    let (library, _) = paged_query_fixture(0);
    let query = Query::new(
        "Thing",
        library.revision_root(),
        QueryLimit::new(1).expect("credit"),
    );
    let names = crate::PublishedNameSearchQuery::admit(query.clone()).expect("name grammar");
    let snapshot = library
        .search_published_names(&names)
        .expect("current published names");
    let page = crate::PublishedNameSearchPage {
        snapshot: snapshot.clone(),
        readiness: crate::PublishedSearchReadiness {
            full_search: crate::FullSearchReadiness::Preparing,
            semantic: crate::PublishedSemanticReadiness::Pending,
        },
    };
    let reply = ReplyDto::new(71, CommandReply::PublishedNameSearch(page.clone()));
    let initial = CommandDto::new(71, Command::Search(query.clone()));
    assert_eq!(admit_reply(&initial, &reply), Ok(()));
    let names_command = CommandDto::new(71, Command::SearchPublishedNames(names.clone()));
    assert_eq!(admit_reply(&names_command, &reply), Ok(()));
    let encoded = serde_json::to_vec(&names_command).expect("command wire");
    assert_eq!(
        CommandDto::decode_against(&encoded, &names_command).expect("exact stage"),
        names_command
    );
    let bytes = serde_json::to_vec(&reply).expect("distinct tagged reply");
    assert_eq!(
        ReplyDto::decode_against(&bytes, &reply).expect("exact stage wire"),
        reply
    );
    let ready = ReplyDto::new(
        71,
        CommandReply::PublishedNameSearch(crate::PublishedNameSearchPage {
            readiness: crate::PublishedSearchReadiness {
                full_search: crate::FullSearchReadiness::Ready,
                semantic: crate::PublishedSemanticReadiness::NotQueried,
            },
            ..page.clone()
        }),
    );
    assert!(
        admit_reply(&initial, &ready).is_err(),
        "initial auto search cannot needlessly degrade a ready projection"
    );
    assert_eq!(
        admit_reply(&names_command, &ready),
        Ok(()),
        "name cursors remain name cursors after readiness"
    );
    for (full_search, semantic) in [
        (
            crate::FullSearchReadiness::Ready,
            crate::PublishedSemanticReadiness::Pending,
        ),
        (
            crate::FullSearchReadiness::Preparing,
            crate::PublishedSemanticReadiness::NotQueried,
        ),
        (
            crate::FullSearchReadiness::Retiring,
            crate::PublishedSemanticReadiness::NotQueried,
        ),
    ] {
        let inconsistent = ReplyDto::new(
            71,
            CommandReply::PublishedNameSearch(crate::PublishedNameSearchPage {
                readiness: crate::PublishedSearchReadiness {
                    full_search,
                    semantic,
                },
                ..page.clone()
            }),
        );
        assert!(admit_reply(&names_command, &inconsistent).is_err());
        assert!(serde_json::to_vec(&inconsistent).is_err());
    }
    let wrong_basis = Query::new(
        query.text(),
        view_state_root(&[("changed".to_owned(), "source".to_owned())]),
        query.limit(),
    );
    let wrong_basis = CommandDto::new(
        71,
        Command::SearchPublishedNames(
            crate::PublishedNameSearchQuery::admit(wrong_basis).expect("same grammar"),
        ),
    );
    assert!(
        admit_reply(&wrong_basis, &reply).is_err(),
        "no current-source substitution"
    );
    let cursor = snapshot.next.expect("bounded page");
    let full_continued = CommandDto::new(71, Command::Search(query.clone().with_cursor(cursor)));
    assert!(
        admit_reply(&full_continued, &reply).is_err(),
        "no cross-stage continuation"
    );
    let full_reply = ReplyDto::new(71, CommandReply::Search(snapshot));
    assert!(admit_reply(&names_command, &full_reply).is_err());
    let vector = reply
        .clone()
        .with_semantic_search_status(crate::SemanticSearchStatus::Available);
    assert!(admit_reply(&names_command, &vector).is_err());
    assert!(
        serde_json::to_vec(&vector).is_err(),
        "name evidence cannot report semantic ranking"
    );
    let mut page = page;
    page.readiness.semantic = crate::PublishedSemanticReadiness::Unavailable {
        reason: crate::SemanticSearchReason::Unconfigured,
    };
    let missing_setup = ReplyDto::new(71, CommandReply::PublishedNameSearch(page))
        .with_semantic_search_status(crate::SemanticSearchStatus::Unavailable {
            reason: crate::SemanticSearchReason::Unconfigured,
        });
    assert_eq!(admit_reply(&names_command, &missing_setup), Ok(()));
    assert!(serde_json::to_vec(&missing_setup).is_ok());
}
