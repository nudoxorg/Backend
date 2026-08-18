//! Red-first specification: **one hit type both planes can produce, and a rule
//! for fusing two copies of it** (S4 prerequisite).
//!
//! # The error class this closes
//!
//! Transparency requires local and remote to produce the *same* item type — a
//! caller holding `Arc<dyn Serve<Symbols>>` must not be able to tell them apart.
//! Today they cannot: the GUI renders `nudox_engine::wire::HitRow` (carrying
//! `sig_preview: Vec<SigToken>`, which needs IR) while the remote produces
//! `heart::Symbol` (name/kind/package only). One of them has to give.
//!
//! Downgrading local to `Symbol` would delete signature previews from the UI —
//! visibly making every row poorer to make remote rows not look poorer. That is
//! the opposite of the goal.
//!
//! So the item is a single [`SymbolHit`] whose signature is **optional**, and
//! `None` means *"the source that produced this row could not supply one"* —
//! never *"this symbol has no signature."* That distinction is the whole point:
//! it is a statement about the **source**, which is exactly what `Residence`
//! already contextualises, and it is what makes the eventual IR-serving work
//! (see `docs/IR-STORAGE-PLAN.md`) a strict upgrade rather than a schema change.
//!
//! # The second gap: precedence-wins throws away enrichment
//!
//! The federating merge resolves a duplicate key by precedence — the
//! higher-precedence source's copy wins wholesale. That is right when the two
//! copies are rival *versions* of a record. It is wrong when they are the same
//! record at different *fidelities*: if local holds a symbol whose signature has
//! not been produced yet and the remote holds the same symbol *with* one,
//! precedence-wins shows the user no signature even though one was available and
//! already on the wire.
//!
//! So `Surface` gains a fusion hook. The default stays precedence-wins (no
//! behaviour change for any surface that does not opt in); `Symbols` overrides
//! it to keep whichever copy actually carries a signature.
//!
//! **Do not weaken these tests to make them pass.**

use heart::identity::PackageId;
use heart::surface::{
    Frame, GenerationId, Located, Residence, Signature, SigToken, SymbolHit, Symbols, Surface,
};
use heart::{Language, Scored, Score, SymbolKind};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn package(tag: u8) -> PackageId {
    PackageId::from_uuid(uuid::Uuid::from_bytes([tag; 16]))
}

fn score(value: f32) -> Score {
    Score::try_new(value).expect("valid score")
}

/// A hit with no signature — what a source that lacks IR can still produce.
fn bare_hit() -> SymbolHit {
    SymbolHit {
        package: package(3),
        path: "serde_json::from_str".into(),
        display_name: "from_str".into(),
        ecosystem: Language::Rust,
        kind: SymbolKind::Function,
        signature: None,
    }
}

/// The same symbol, with a rendered signature.
fn signed_hit() -> SymbolHit {
    SymbolHit {
        signature: Some(Signature::new(vec![
            SigToken::keyword("fn"),
            SigToken::space(),
            SigToken::ident("from_str"),
            SigToken::punct("("),
            SigToken::ident("s"),
            SigToken::punct(":"),
            SigToken::space(),
            SigToken::ty("&str", None),
            SigToken::punct(")"),
        ])),
        ..bare_hit()
    }
}

// ---------------------------------------------------------------------------
// 1. The item type
// ---------------------------------------------------------------------------

/// `None` is a claim about the *source*, not about the symbol. A reader must be
/// able to distinguish "we don't have it" from "there isn't one", because the
/// first is a prompt to fetch and the second is not.
#[test]
fn an_absent_signature_means_the_source_could_not_supply_one() {
    let bare = bare_hit();
    assert!(bare.signature.is_none());
    assert!(
        !bare.has_signature(),
        "a bare hit reports that it carries no signature"
    );

    let signed = signed_hit();
    assert!(signed.has_signature());
    assert_eq!(
        signed.signature.as_ref().map(Signature::token_count),
        Some(9)
    );
}

/// The dedup key stays instance-independent (§0.7 of the contract doc) and does
/// not depend on fidelity — a bare hit and a signed hit for the same symbol are
/// one identity, or fusion could never fire.
#[test]
fn fidelity_does_not_change_identity() {
    let bare = Scored::new(bare_hit(), score(0.5));
    let signed = Scored::new(signed_hit(), score(0.9));
    assert_eq!(
        Symbols::key(&bare),
        Symbols::key(&signed),
        "the same symbol at two fidelities must be one key"
    );
}

/// A signature token that links to another symbol must carry a portable target.
/// `StableReference`'s frozen `F:<eco>/<pkg>#<hex>` grammar is instance-
/// independent, so a remote-produced signature stays navigable locally.
#[test]
fn signature_type_tokens_can_link_across_packages() {
    let target = heart::query::StableReference::parse("F:rust/serde#0a1b2c3d")
        .expect("the frozen grammar parses");
    let token = SigToken::ty("Value", Some(target.clone()));
    match &token {
        SigToken::Ty { target: Some(t), .. } => assert_eq!(t, &target),
        other => panic!("expected a linked type token, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 2. Wire form
// ---------------------------------------------------------------------------

#[test]
fn a_hit_round_trips_with_and_without_a_signature() {
    for hit in [bare_hit(), signed_hit()] {
        let frame: Frame<Symbols> = Frame::Item(Located::new(
            Scored::new(hit.clone(), score(0.7)),
            Residence::Remote {
                generation: GenerationId(2),
            },
        ));
        let line = serde_json::to_string(&frame).expect("serializes");
        let back: Frame<Symbols> = serde_json::from_str(&line).expect("deserializes");
        assert_eq!(frame, back, "wire round-trip changed the hit");
    }
}

/// An absent signature must not cost wire bytes — most rows from a source
/// without IR will have none, and a `"signature":null` on every line is pure
/// overhead on the hottest path in the system.
#[test]
fn an_absent_signature_is_omitted_from_the_wire() {
    let json = serde_json::to_string(&bare_hit()).expect("serializes");
    assert!(
        !json.contains("signature"),
        "an absent signature must be skipped, not encoded as null: {json}"
    );
}

// ---------------------------------------------------------------------------
// 3. Fusion — the rule for two copies of one identity
// ---------------------------------------------------------------------------

/// The default must be exactly today's behaviour — higher precedence wins
/// wholesale — so adding this hook changes nothing for surfaces that do not
/// opt in.
#[test]
fn the_default_fusion_is_precedence_wins() {
    // `Packages` does not override fusion.
    let a = Scored::new(sample_package_hit("alpha"), score(0.9));
    let b = Scored::new(sample_package_hit("beta"), score(0.1));
    let fused = heart::surface::Packages::fuse(a.clone(), b);
    assert_eq!(
        fused.value.name, a.value.name,
        "without an override, the higher-precedence copy wins whole"
    );
}

/// The case that matters: local holds the symbol but has not produced a
/// signature; the remote holds the same symbol *with* one. Precedence-wins would
/// show the user nothing. Fusion must keep the signature.
#[test]
fn fusion_keeps_a_signature_the_winning_copy_lacks() {
    let winner = Scored::new(bare_hit(), score(0.9)); // higher precedence, poorer
    let loser = Scored::new(signed_hit(), score(0.2)); // lower precedence, richer

    let fused = Symbols::fuse(winner, loser);
    assert!(
        fused.value.has_signature(),
        "fusion must not discard a signature the winning copy lacked"
    );
}

/// Fusion must not *downgrade*: a winner that already has a signature keeps its
/// own, rather than adopting the loser's stale one.
#[test]
fn fusion_never_replaces_a_signature_the_winner_already_has() {
    let mut richer = signed_hit();
    richer.display_name = "winner".into();
    let winner = Scored::new(richer, score(0.9));

    let mut other = signed_hit();
    other.display_name = "loser".into();
    let loser = Scored::new(other, score(0.2));

    let fused = Symbols::fuse(winner, loser);
    assert_eq!(
        fused.value.display_name, "winner",
        "the higher-precedence copy still owns every field it can supply"
    );
}

/// Everything except the enriched field comes from the winner — fusion is
/// precedence-wins *plus* backfill, not a free-for-all field merge.
#[test]
fn fusion_takes_all_other_fields_from_the_winner() {
    let mut winner_hit = bare_hit();
    winner_hit.display_name = "winner".into();
    let winner = Scored::new(winner_hit, score(0.9));
    let loser = Scored::new(signed_hit(), score(0.2));

    let fused = Symbols::fuse(winner, loser);
    assert_eq!(fused.value.display_name, "winner");
    assert_eq!(fused.score, score(0.9), "the winner's score is kept");
    assert!(fused.value.has_signature(), "and the signature is backfilled");
}

/// Fusion must be idempotent — fusing a value with itself changes nothing.
/// Without this, a supersede arriving twice could oscillate.
#[test]
fn fusion_is_idempotent() {
    let hit = Scored::new(signed_hit(), score(0.9));
    let once = Symbols::fuse(hit.clone(), hit.clone());
    assert_eq!(once.value, hit.value);
    let twice = Symbols::fuse(once.clone(), once.clone());
    assert_eq!(twice.value, hit.value);
}

// ---------------------------------------------------------------------------
// 4. The merge must actually USE fusion
// ---------------------------------------------------------------------------

/// Without this, the hook is decorative. The end-to-end shape: a local source
/// (precedence 0) supplies the symbol bare, a remote source (precedence 1)
/// supplies it signed, and the row the consumer ends up holding must carry the
/// signature — from the *local* row's perspective, enriched.
#[tokio::test]
async fn a_merged_duplicate_is_fused_not_merely_replaced() {
    use heart::access::SourceId;
    use heart::surface::{Gen, answer_channel, merge};

    let (tx_local, local) = answer_channel::<Symbols>(8, Gen(1));
    let (tx_remote, remote) = answer_channel::<Symbols>(8, Gen(1));

    let (answer, pump) = merge::<Symbols>(
        Gen(1),
        vec![
            (SourceId::from_uuid(uuid::Uuid::from_bytes([1; 16])), local),
            (SourceId::from_uuid(uuid::Uuid::from_bytes([2; 16])), remote),
        ],
    );
    let pumping = tokio::spawn(pump);

    // The remote answers first with the richer copy...
    tx_remote
        .item(
            Scored::new(signed_hit(), score(0.4)),
            Residence::Remote {
                generation: GenerationId(1),
            },
        )
        .expect("emit");
    let first = answer.recv().await.expect("the remote row");
    assert!(matches!(&first, Frame::Item(l) if l.value.value.has_signature()));

    // ...then the higher-precedence local copy arrives, bare. It supersedes —
    // but must not strip the signature the consumer already has.
    tx_local
        .item(Scored::new(bare_hit(), score(0.9)), Residence::Local)
        .expect("emit");
    tx_local.end(heart::surface::Summary::complete(1)).ok();
    tx_remote.end(heart::surface::Summary::complete(1)).ok();

    let mut frames = vec![first];
    while let Some(f) = answer.recv().await {
        frames.push(f);
    }
    pumping.await.expect("pump completes");

    let last_item = frames
        .iter()
        .filter_map(|f| match f {
            Frame::Item(l) => Some(l),
            _ => None,
        })
        .next_back()
        .expect("at least one item");

    assert_eq!(
        last_item.residence,
        Residence::Local,
        "the higher-precedence copy still wins the row"
    );
    assert!(
        last_item.value.value.has_signature(),
        "superseding with a bare local copy must not strip a signature the \
         consumer was already shown — that is a visible downgrade mid-query"
    );
}

fn sample_package_hit(name: &str) -> heart::PackageHit {
    heart::PackageHit {
        id: package(9),
        name: name.into(),
        ..Default::default()
    }
}
