//! Red-first specification for the concrete search surfaces (S2).
//!
//! These are the `Surface` impls the *server* answers and the *client* requests.
//! Defining them once in `heart` is what lets `RemoteClient`'s blanket
//! `impl<S: Surface> Serve<S>` talk to the real `/search` route without a
//! hand-written method per endpoint — the gap that left `NudoxClient` covering
//! 4 of the server's 17 routes.
//!
//! # The hole these close
//!
//! `heart::query::Query` carries a `target: Target` enum, but `QueryEngine` has
//! a single associated `Hit` type, so the pairing of "what you asked for" to
//! "what you get back" is checked at *runtime* — `search.rs:104` literally
//! rejects a request whose `target` does not match the route. Under `Surface`
//! the pairing is an associated type, so the mismatch is a compile error and
//! that runtime check has nothing left to reject.
//!
//! **Do not weaken a test to make it pass.**

use heart::query::{Query, Target};
use heart::surface::{Frame, Located, Residence, Summary, Surface};
use heart::surface::{Packages, SearchNote, SigToken, Signature, SymbolHit, Symbols, Usages};
use heart::{PackageHit, Scored};

// ---------------------------------------------------------------------------
// 1. Each surface names the route it answers
// ---------------------------------------------------------------------------

/// `PATH` is the single source of truth for the route. Client and server both
/// read it from here, so they cannot drift — today the route string is written
/// once in `router.rs` and again in every client method.
#[test]
fn each_surface_names_the_route_it_answers() {
    assert_eq!(Symbols::PATH, "/search");
    assert_eq!(Packages::PATH, "/packages/search");
    assert_eq!(Usages::PATH, "/usages");

    assert_eq!(Symbols::NAME, "symbols");
    assert_eq!(Packages::NAME, "packages");
    assert_eq!(Usages::NAME, "usages");
}

/// All three share the one query algebra as their request type. Keeping
/// `Request = Query` is what makes S2 wire-compatible: the request body on the
/// network is byte-identical to what the handlers already deserialize. Only the
/// *response* envelope changes.
#[test]
fn every_search_surface_takes_the_one_query_algebra() {
    fn assert_request_is_query<S: Surface<Request = Query>>() {}
    assert_request_is_query::<Symbols>();
    assert_request_is_query::<Packages>();
    assert_request_is_query::<Usages>();
}

// ---------------------------------------------------------------------------
// 2. The item type is a function of the surface — the runtime check, deleted
// ---------------------------------------------------------------------------

/// This is the whole point. `Symbols` yields `Scored<SymbolHit>` and `Packages`
/// yields `Scored<PackageHit>` *by type*. A caller cannot ask the symbol surface
/// for package hits, so `search.rs:104`'s "target must be Symbols for /search"
/// guard becomes unreachable rather than load-bearing.
///
/// `SymbolHit`, not `Symbol`: `Symbols::Item` moved onto the one item type
/// both the local engine and a remote server can produce, since `Symbol`
/// carries no notion of an optional signature and downgrading local rows to
/// it would delete signature previews from the UI just to make remote rows
/// not look poorer (`hit_fusion.rs`'s module doc comment; `heart::Symbol`
/// remains the *internal* engine-side record `SymbolHit` is projected from).
#[test]
fn the_item_type_is_determined_by_the_surface() {
    fn assert_symbol_items<S: Surface<Item = Scored<SymbolHit>>>() {}
    fn assert_package_items<S: Surface<Item = Scored<PackageHit>>>() {}
    assert_symbol_items::<Symbols>();
    assert_package_items::<Packages>();
}

/// Dedup identity ignores the score — the federating merge suppresses a
/// duplicate row across sources on this key, and the same symbol ranked
/// differently by two sources is still one row.
///
/// (This test originally also asserted the key *was* `Symbol::id`. That was
/// wrong and is disproven by §2b below: `SymbolId` is instance-salted, so
/// keying on it makes cross-source dedup structurally impossible.)
#[test]
fn symbols_dedup_ignores_the_score() {
    let symbol = sample_symbol();
    let hit_a = Scored::new(symbol.clone(), score(0.9));
    let hit_b = Scored::new(symbol, score(0.1));

    assert_eq!(
        Symbols::key(&hit_a),
        Symbols::key(&hit_b),
        "the same symbol at two scores is one identity"
    );
}

// ---------------------------------------------------------------------------
// 2b. The dedup key must be INSTANCE-INDEPENDENT
// ---------------------------------------------------------------------------

/// The blocker this closes, and it is a silent one.
///
/// `heart::Symbol::id` (`SymbolId`) is minted by
/// `EntryUri::symbol_id(instance_token)` as `UUIDv5(SYMBOL_ns, instance_token
/// ‖ 0 ‖ package_id/path…)`, and `instance_token` is `"{org}/{db}"` read from
/// **server configuration** (`index/server/config.rs:527`). Its own doc
/// comment is explicit that this is deliberate: *"salted with the TerminusDB
/// instance so two instances of the same corpus don't share ids."*
///
/// That makes `SymbolId` unusable as `Surface::Key`, and it is exactly why
/// `Symbols::Item`'s wire payload, [`SymbolHit`], does not carry an id field
/// at all (see [`SymbolHit`]'s own doc comment) — there is nothing left on
/// the wire item for a future reader to reach for by mistake. That leaves
/// this test with a stronger property to demonstrate than the original
/// (which only had one instance-salted field to vary): two instances of the
/// same corpus must agree on the dedup key even when *every* field besides
/// `(package, path)` differs between their copies — display name, kind, and
/// fidelity (one instance has produced a signature, the other has not).
/// `(package, path)` is the *only* pair either plane can compute
/// independently without an instance-local salt, so it is the only pair
/// allowed to participate in the key at all.
#[test]
fn the_symbol_key_is_the_same_across_two_index_instances() {
    // The same logical symbol, as two instances of the same corpus would
    // build it: identical package and fully-qualified path, but every other
    // field diverges the way two independent instances plausibly could.
    let from_instance_a = Scored::new(sample_symbol(), score(0.9));
    let mut other = sample_symbol();
    other.display_name = "aka_from_str".into();
    other.kind = heart::SymbolKind::Other;
    other.signature = Some(Signature::new(vec![SigToken::keyword("fn")]));
    let from_instance_b = Scored::new(other, score(0.4));

    assert_ne!(
        from_instance_a.value, from_instance_b.value,
        "fixture must actually model two divergent, non-identity copies"
    );
    assert_eq!(
        Symbols::key(&from_instance_a),
        Symbols::key(&from_instance_b),
        "two instances of the same corpus must agree on a symbol's dedup key \
         even when every non-identity field differs — only (package, path) \
         may ever participate in the key"
    );
}

/// The converse: genuinely different symbols must not collide. A key that
/// ignored the package would merge same-named symbols from different crates.
#[test]
fn symbols_in_different_packages_do_not_share_a_key() {
    let a = Scored::new(sample_symbol(), score(0.9));
    let mut elsewhere = sample_symbol();
    elsewhere.package = heart::identity::PackageId::from_uuid(uuid::Uuid::from_bytes([0x5C; 16]));
    let b = Scored::new(elsewhere, score(0.9));

    assert_ne!(
        Symbols::key(&a),
        Symbols::key(&b),
        "the same symbol name in two packages must remain two identities"
    );
}

/// And two different symbols within one package must not collide either.
#[test]
fn different_symbols_in_one_package_do_not_share_a_key() {
    let a = Scored::new(sample_symbol(), score(0.9));
    let mut sibling = sample_symbol();
    sibling.display_name = "to_string".into();
    sibling.path = "serde_json::to_string".into();
    let b = Scored::new(sibling, score(0.9));

    assert_ne!(Symbols::key(&a), Symbols::key(&b));
}

// ---------------------------------------------------------------------------
// 3. Wire form — what the server writes and the client reads
// ---------------------------------------------------------------------------

/// A frame carrying a real `Scored<SymbolHit>` must round-trip. This is the pin
/// that stops the server's writer and the client's reader drifting: both name
/// `Frame<Symbols>` and there is no second, hand-maintained DTO.
#[test]
fn a_symbol_frame_round_trips_over_the_wire() {
    let frame: Frame<Symbols> = Frame::Item(Located::new(
        Scored::new(sample_symbol(), score(0.75)),
        Residence::Remote {
            generation: heart::surface::GenerationId(4),
        },
    ));

    let line = serde_json::to_string(&frame).expect("frame serializes");
    let back: Frame<Symbols> = serde_json::from_str(&line).expect("frame deserializes");
    assert_eq!(frame, back);

    assert!(
        line.starts_with(r#"{"item":"#),
        "expected an item-tagged frame, got {line}"
    );
}

/// Residence must survive the wire. A hit the server served from its own corpus
/// arrives at the client tagged `Remote` — and that tag is the only thing that
/// tells the GUI this row needs the network to be re-served.
#[test]
fn residence_survives_the_wire_for_a_real_hit() {
    let frame: Frame<Symbols> = Frame::Item(Located::new(
        Scored::new(sample_symbol(), score(0.5)),
        Residence::Remote {
            generation: heart::surface::GenerationId(11),
        },
    ));
    let back: Frame<Symbols> =
        serde_json::from_str(&serde_json::to_string(&frame).unwrap()).unwrap();

    match back {
        Frame::Item(located) => assert_eq!(
            located.residence,
            Residence::Remote {
                generation: heart::surface::GenerationId(11)
            }
        ),
        other => panic!("expected an item, got {other:?}"),
    }
}

/// The terminal frame carries completeness, so a client can tell a full answer
/// from one served while a source was down.
#[test]
fn a_terminal_frame_round_trips_its_completeness() {
    let frame: Frame<Symbols> = Frame::End(Summary::partial(3, 3, Some(40)));
    let back: Frame<Symbols> =
        serde_json::from_str(&serde_json::to_string(&frame).unwrap()).unwrap();
    match back {
        Frame::End(summary) => assert!(!summary.is_complete()),
        other => panic!("expected End, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 4. Notes — the out-of-band metadata a search emits
// ---------------------------------------------------------------------------

/// `SearchNote` generalizes what `SearchEvent::{Latency, SectionState}` carried
/// locally, so the remote can say the same things the local engine already says.
/// Without it, a remote answer cannot report "the semantic index is still
/// building" and the GUI has to infer coverage from an empty result set — which
/// is exactly the false "we searched and found nothing" the local protocol added
/// `SectionState` to kill.
#[test]
fn search_notes_round_trip() {
    let notes = [
        SearchNote::Latency { millis: 42 },
        SearchNote::Coverage {
            covered: 10,
            total: Some(100),
        },
        SearchNote::Coverage {
            covered: 10,
            total: None,
        },
    ];
    for note in notes {
        let frame: Frame<Symbols> = Frame::Note(note.clone());
        let back: Frame<Symbols> =
            serde_json::from_str(&serde_json::to_string(&frame).unwrap()).unwrap();
        assert_eq!(frame, back);
    }
}

// ---------------------------------------------------------------------------
// 5. Backwards compatibility of the request half
// ---------------------------------------------------------------------------

/// S2 changes the response envelope, not the request. A `Query` built for the
/// old route must serialize identically — otherwise this is a two-sided break
/// instead of a one-sided one, and every existing caller breaks at once.
#[test]
fn the_request_body_is_unchanged_from_the_pre_surface_wire() {
    let query = Query {
        target: Target::Symbols,
        text: "deserialize".to_owned(),
        ..sample_query()
    };
    let json = serde_json::to_string(&query).expect("query serializes");
    let back: Query = serde_json::from_str(&json).expect("query deserializes");
    assert_eq!(query, back);
    assert!(
        json.contains(r#""text":"deserialize""#),
        "the request shape must be untouched, got {json}"
    );
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn score(value: f32) -> heart::Score {
    heart::Score::try_new(value).expect("a valid score")
}

fn sample_query() -> Query {
    Query {
        target: Target::Symbols,
        text: String::new(),
        scope: Default::default(),
        rank: Default::default(),
        mode: Default::default(),
        routing: Default::default(),
        session: None,
        at: None,
        page: Default::default(),
        query_id: None,
    }
}

fn sample_symbol() -> SymbolHit {
    SymbolHit {
        package: heart::identity::PackageId::from_uuid(uuid::Uuid::from_bytes([3; 16])),
        path: "serde_json::from_str".into(),
        display_name: "from_str".into(),
        ecosystem: heart::Language::Rust,
        kind: heart::SymbolKind::Function,
        signature: None,
        reference: None,
    }
}
