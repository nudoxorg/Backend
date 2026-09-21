#![allow(clippy::expect_used)]

use crate::canonical::{
    object_version, symbol_key, view_key, view_state_root, view_version_preimage,
};
use crate::{
    AuthorityScopeClaim, Basis, Command, CommandDto, CommandReply, CompleteViewProjection,
    CoverageCapability, Cursor, CursorEvent, DTO_VERSION, Document, DocumentQuery, EventDto,
    Fragment, Freshness, Frontier, GraphNeighborhoodQuery, NameQuery, Outline, OutlineNode,
    OutlineQuery, Query, QueryLimit, ReplyDto, RequestAdmissionError, Row, RowId, ScopeRoot,
    SemanticObject, SnapshotPageDto, SourceAvailability, SourceExcerpt, SourceExcerptExtent,
    SourceLocation, SubscriptionDto, ViewDelta, ViewDto, ViewPageCursor, ViewRoot, ViewSnapshot,
    ViewStateRoot, ViewError, WireCertificate, WireClaim, WireSchema, RowIdentityPreimage,
    MAX_ROW_IDENTITY_PREIMAGE_BYTES, admit_reply,
    admit_reply_with_capability, admit_request, encode_id,
};

fn capability(object: SemanticObject) -> CoverageCapability {
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

fn certificate(root: &ViewRoot) -> WireCertificate {
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
        .with_identity_preimage(
            RowIdentityPreimage::try_new(preimage).expect("bounded preimage"),
        );
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
        },
    )
    .with_certificate(forged);
    let forged_encoded = serde_json::to_vec(&forged_view).expect("encode forged view");
    assert!(ViewDto::decode_with_certificate(&forged_encoded, Some(capability(basis.object)))
        .is_err());

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
        },
    )
    .with_certificate(mixed);
    let mixed_encoded = serde_json::to_vec(&mixed_view).expect("encode mixed view");
    assert!(ViewDto::decode_with_certificate(&mixed_encoded, Some(capability(basis.object)))
        .is_err());

    let oversized = certificate(&root).with_claim(WireClaim::RowIdentity {
        schema: WireSchema::Symbol,
        id: encode_id(symbol.as_bytes()),
        preimage: "x".repeat(MAX_ROW_IDENTITY_PREIMAGE_BYTES + 1),
    });
    assert!(oversized
        .row_identity_preimage(WireSchema::Symbol, &encode_id(symbol.as_bytes()))
        .is_err());
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
    assert!(RowIdentityPreimage::try_new(
        &"x".repeat(MAX_ROW_IDENTITY_PREIMAGE_BYTES + 1)
    )
    .is_err());
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
            package: crate::package_key("pkg"),
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
        Command::Add { package },
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
    let cursor = continuation.cursor();
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
        recipe: encode_id(cursor.recipe().as_bytes()),
        version: encode_id(cursor.version().as_bytes()),
        branch: encode_id(cursor.branch().as_bytes()),
        log: encode_id(cursor.log().as_bytes()),
        schema: cursor.schema(),
        root: encode_id(cursor.root().as_bytes()),
        sequence: cursor.sequence(),
    });
    let expected = CommandDto::new(
        43,
        Command::GraphQuery(request.with_continuation(continuation)),
    )
    .with_certificate(WireCertificate {
        claims: claims.into_boxed_slice(),
    });
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

    let view = ViewDto::new(
        13,
        ViewSnapshot {
            root: ViewRoot::empty_checked(
                view_key(b"view-dto"),
                basis,
                Frontier::new(basis.branch, basis.log, basis.schema, source_root, 0),
                capability(basis.object),
            )
            .expect("checked view root"),
            freshness: Freshness::Stale {
                observed: source_root,
            },
            next: None,
            graph_relations: None,
        },
    );
    let encoded = serde_json::to_vec(&view).expect("encode view");
    assert!(serde_json::from_slice::<ViewDto>(&encoded).is_err());
    let decoded = ViewDto::decode_against(&encoded, &view).expect("decode view");
    assert_eq!(decoded, view);
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

    let reply = ReplyDto::error(2, "error");
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
