//! Red-first specification: **a symbol identity both planes can produce, and a
//! consumer can navigate with** (contract §2, task S4).
//!
//! # Two defects
//!
//! **1. The local plane cannot produce a `SymbolHit` at all.** `Symbols::key`
//! is `(PackageId, path)`, and `surfaces.rs` defends that at length: `PackageId`
//! is a deterministic UUIDv5 over package coordinates, *not* instance-salted
//! the way `SymbolId` is, so "two instances of the same corpus therefore
//! agree."
//!
//! Every word of that is true, and it reasons only about two *index* instances.
//! `PackageId` is derived from **`RegistryOrigin` + name + concrete version**
//! (`heart/package/coordinates.rs:35-41`). The engine holds
//! `PackageLineageId { ecosystem, name }`, **deliberately version-free** —
//! `nudox-engine/src/versions/mod.rs:6-11` says it "names the lineage
//! (`cargo:axum`), which is exactly the identity that has to stay constant
//! across releases" — and `RegistryOrigin` does not appear anywhere in
//! `nudox-engine`. Task #16 removed the *salt* from the key and never checked
//! *availability*; this is the same defect class one step on.
//!
//! **2. Remote hits are already non-navigable — this is shipping today.**
//! `gui/src/stores/search_model.rs:607` (`remote_symbol_key`) fabricates a
//! `SymbolKey` by copying a UUID's 16 bytes into an `IntroId`'s first 16 and
//! zeroing the rest. A real `IntroId` is `blake3("nudox.intro.v5" ‖ lineage ‖
//! kind ‖ path ‖ name ‖ disambiguator)` (`ir/model/src/intro.rs:200-214`), so
//! it cannot match a sealed entry. `resolve_symbol`
//! (`nudox-engine/src/doc/mod.rs:150-177`) does an exact lookup —
//! `pkg.view().entry(key.intro)` — and returns `SymbolNotFound`. **Clicking a
//! remote search result fails.** The function's own doc comment concedes there
//! is "no *true* `SymbolKey` to recover" and synthesizes one regardless.
//!
//! # Why the key does NOT change
//!
//! The tempting fix is a version-free key both planes hold outright. It does
//! not work: `heart::Symbol` — all the index has when it builds a hit
//! (`precise_source_answer` does `scored.map(SymbolHit::from)`) — carries
//! `package: PackageId` and **no package name string** at all
//! (`heart/symbol.rs:22-38`). So a lineage-stem key would need a catalog lookup
//! per hit on the remote side, and the mapping problem would simply move.
//!
//! Since some derivation is required either way, it belongs on the side that
//! does not work today. Keeping `(PackageId, path)` means the **index — the
//! more complex, more heavily tested plane — does not change at all**, dedup
//! stays version-aware, and `merge`'s already-verified semantics are untouched.
//! All the new work lands in the engine, which currently does not participate.
//!
//! # What this file pins
//!
//! * `Language::from_lineage_tag` / `Language::lineage_tag` and
//!   `RegistryOrigin::default_for` — the two small, closed mappings that let the
//!   engine turn `PackageLineageId { ecosystem: "cargo", name: "axum" }` plus a
//!   resident version into the same `PackageId` the index derives. **Total over
//!   every `Language`**, with no silent fallback: a wrong or missing mapping
//!   would break dedup silently, which is precisely the failure mode task #16
//!   existed to remove, so it is pinned rather than trusted.
//! * `SymbolHit::reference: Option<StableReference>` — navigable identity.
//!
//! `StableReference` is not invented here. `heart::query` already froze the
//! grammar `F:<eco>/<pkg>#<hex>` and the `Usages` surface already keys on it.
//! Its three components map exactly onto the engine's `SymbolKey`
//! (`ir::change::StableRef { package: PackageLineageId { ecosystem, name },
//! intro: IntroId }`), so the conversion is total both ways — and unlike
//! `SymbolId` it is **content-derived, not instance-salted**, which is the
//! property contract §0.7 actually asked for.
//!
//! `Option` is a statement about the **source**, never the symbol — the same
//! doctrine `SymbolHit::signature` already documents. A row with no `reference`
//! is not navigable *yet*; it becomes navigable when the index serves IR, with
//! no schema change. That is honest. Fabricating a key that always fails is not.
//!
//! **Do not weaken these tests to make them pass.** In particular, do not give
//! the ecosystem mappings a catch-all arm — the whole point is that a new
//! `Language` must fail to compile until someone decides its registry.

use heart::identity::PackageId;
use heart::package::Coordinates;
use heart::query::StableReference;
use heart::surface::{Surface, SymbolHit, Symbols};
use heart::{Language, PackageVersion, RegistryOrigin, Score, Scored, SymbolKind};
use smol_str::SmolStr;
use strum::IntoEnumIterator as _;

/// `axum 0.8.1`, spelled the way each plane would spell it.
fn axum_name() -> heart::package::PackageName {
    heart::package::PackageName::from_canonical(Language::Rust, "axum", "axum")
}

fn axum_version() -> PackageVersion {
    PackageVersion::Cargo(semver::Version::parse("0.8.1").expect("valid semver"))
}

fn score(value: f32) -> Score {
    Score::try_new(value).expect("valid score")
}

// ---------------------------------------------------------------------------
// 1. The two mappings that let the engine derive a `PackageId`
// ---------------------------------------------------------------------------

/// Every `Language` must have a registry. A catch-all arm would let a new
/// ecosystem silently inherit someone else's registry and mint wrong
/// `PackageId`s — which breaks dedup with no error anywhere.
#[test]
fn every_language_has_a_default_registry() {
    for language in Language::iter() {
        let origin = RegistryOrigin::default_for(language);
        assert!(
            !matches!(origin, RegistryOrigin::Custom { .. }),
            "{language:?} must map to a concrete registry, not Custom"
        );
    }
}

/// The registries must be distinct per ecosystem, except where the codebase
/// deliberately shares one: `Cpp` is registry-less and git-native (RL-1).
#[test]
fn registries_match_the_documented_ecosystem_planes() {
    assert_eq!(
        RegistryOrigin::default_for(Language::Rust),
        RegistryOrigin::CratesIo
    );
    assert_eq!(
        RegistryOrigin::default_for(Language::Typescript),
        RegistryOrigin::NpmPublic
    );
    assert_eq!(
        RegistryOrigin::default_for(Language::Python),
        RegistryOrigin::PyPi
    );
    assert_eq!(
        RegistryOrigin::default_for(Language::Go),
        RegistryOrigin::GoProxy
    );
    assert_eq!(
        RegistryOrigin::default_for(Language::Java),
        RegistryOrigin::MavenCentral
    );
    assert_eq!(
        RegistryOrigin::default_for(Language::CSharp),
        RegistryOrigin::NuGet
    );
    assert_eq!(
        RegistryOrigin::default_for(Language::Cpp),
        RegistryOrigin::Git,
        "cpp is the registry-less, git-native plane (RL-1)"
    );
}

/// The engine names ecosystems with its own producer tags (`cargo`, `npm`, …
/// — see `index/server/coordination/compile_inprocess.rs:371-382`), which are
/// NOT `Language`'s own wire tokens (`rust`, `typescript`, …). The two
/// vocabularies genuinely differ, so the mapping must round-trip exactly or the
/// engine will look up the wrong ecosystem.
#[test]
fn the_lineage_tag_mapping_round_trips_for_every_language() {
    for language in Language::iter() {
        let tag = language.lineage_tag();
        assert_eq!(
            Language::from_lineage_tag(tag),
            Some(language),
            "{language:?} must round-trip through its lineage tag {tag:?}"
        );
    }
}

/// The specific spellings the engine actually emits. Pinning these stops the
/// mapping from being "fixed" into agreement with `Language`'s wire tokens,
/// which would break every real lineage id in the corpus.
#[test]
fn lineage_tags_match_what_the_engine_emits() {
    assert_eq!(Language::from_lineage_tag("cargo"), Some(Language::Rust));
    assert_eq!(
        Language::from_lineage_tag("npm"),
        Some(Language::Typescript)
    );
    assert_eq!(Language::from_lineage_tag("pypi"), Some(Language::Python));
    assert_eq!(Language::from_lineage_tag("go"), Some(Language::Go));
    assert_eq!(Language::from_lineage_tag("maven"), Some(Language::Java));
    assert_eq!(Language::from_lineage_tag("nuget"), Some(Language::CSharp));
    assert_eq!(Language::from_lineage_tag("cpp"), Some(Language::Cpp));
}

/// An unknown tag must be `None`, never a guess. A producer emitting an
/// ecosystem this build does not know about must fail loudly at the conversion
/// rather than mint a `PackageId` under the wrong registry.
#[test]
fn an_unknown_lineage_tag_is_rejected() {
    assert_eq!(Language::from_lineage_tag("hackage"), None);
    assert_eq!(Language::from_lineage_tag(""), None);
    assert_eq!(
        Language::from_lineage_tag("rust"),
        None,
        "`rust` is Language's own wire token, not a lineage tag — accepting it \
         here would hide a caller passing the wrong vocabulary"
    );
}

/// THE test. The engine, starting from lineage data plus a resident version,
/// must derive **bit-identical** `PackageId`s to the index, which starts from
/// full coordinates. If these ever disagree, dedup silently never fires and
/// every synced package renders twice — nothing errors.
#[test]
fn both_planes_derive_the_same_package_id() {
    // What the index has.
    let index_side = Coordinates {
        origin: RegistryOrigin::CratesIo,
        name: axum_name(),
        version: axum_version(),
    }
    .id();

    // What the engine has: the lineage tag `cargo`, the stem `axum`, and the
    // version of the copy it actually loaded.
    let language = Language::from_lineage_tag("cargo").expect("known ecosystem");
    let engine_side = Coordinates {
        origin: RegistryOrigin::default_for(language),
        name: heart::package::PackageName::from_canonical(language, "axum", "axum"),
        version: axum_version(),
    }
    .id();

    assert_eq!(
        engine_side, index_side,
        "the two planes must mint the same package identity from the data each \
         of them actually holds"
    );
}

// ---------------------------------------------------------------------------
// 2. Navigable identity
// ---------------------------------------------------------------------------

fn hit(package: PackageId, path: &str, reference: Option<StableReference>) -> SymbolHit {
    SymbolHit {
        package,
        path: SmolStr::new(path),
        display_name: SmolStr::new(path.rsplit("::").next().unwrap_or(path)),
        ecosystem: Language::Rust,
        kind: SymbolKind::Function,
        signature: None,
        reference,
    }
}

fn package_id(tag: u8) -> PackageId {
    PackageId::from_uuid(uuid::Uuid::from_bytes([tag; 16]))
}

fn reference_for(package: &str, intro_hex: &str) -> StableReference {
    StableReference::parse(&format!("F:rust/{package}#{intro_hex}"))
        .expect("fixture must build a valid reference")
}

const INTRO_A: &str = "aa11bb22cc33dd44ee55ff6600778899aabbccddeeff00112233445566778899";
const INTRO_B: &str = "bb11bb22cc33dd44ee55ff6600778899aabbccddeeff00112233445566778899";

/// A source without IR must still be able to answer — `reference` is optional
/// exactly as `signature` is, and for the same reason.
#[test]
fn a_hit_without_a_reference_is_valid() {
    let bare = hit(package_id(1), "axum::routing::get", None);
    assert!(bare.reference.is_none());
}

/// Fidelity must not change identity, or `fuse` never gets a chance to run and
/// a navigable copy and a bare copy render as two separate rows.
#[test]
fn a_reference_does_not_change_dedup_identity() {
    let bare = Scored::new(hit(package_id(1), "axum::routing::get", None), score(0.5));
    let navigable = Scored::new(
        hit(
            package_id(1),
            "axum::routing::get",
            Some(reference_for("axum", INTRO_A)),
        ),
        score(0.9),
    );

    assert_eq!(Symbols::key(&bare), Symbols::key(&navigable));
}

/// THE payoff. The local plane has IR and can navigate; the index has ranking
/// and reach. Whichever wins on precedence, the fused row must be openable —
/// that is what makes a federated row strictly better than either source's.
#[test]
fn a_bare_winner_adopts_the_losers_navigable_reference() {
    let winner = Scored::new(hit(package_id(1), "axum::routing::get", None), score(0.9));
    let loser = Scored::new(
        hit(
            package_id(1),
            "axum::routing::get",
            Some(reference_for("axum", INTRO_A)),
        ),
        score(0.2),
    );

    let fused = Symbols::fuse(winner, loser).value;
    assert_eq!(
        fused.reference,
        Some(reference_for("axum", INTRO_A)),
        "a row that could be opened before the merge must still be openable \
         after it"
    );
}

/// Backfill, never overwrite. Nothing about lower precedence makes the loser's
/// identity more trustworthy — and adopting it would silently retarget a link.
#[test]
fn fusion_never_replaces_a_reference_the_winner_already_has() {
    let winner = Scored::new(
        hit(
            package_id(1),
            "axum::routing::get",
            Some(reference_for("axum", INTRO_A)),
        ),
        score(0.9),
    );
    let loser = Scored::new(
        hit(
            package_id(1),
            "axum::routing::get",
            Some(reference_for("axum", INTRO_B)),
        ),
        score(0.2),
    );

    let fused = Symbols::fuse(winner, loser).value;
    assert_eq!(
        fused.reference,
        Some(reference_for("axum", INTRO_A)),
        "the winner's own identity must win; backfill only fills a hole"
    );
}

/// `Surface::fuse`'s documented contract — `merge` may call it repeatedly as
/// duplicates arrive, so a non-idempotent override would oscillate a row rather
/// than converge on a stable, maximally-enriched value.
#[test]
fn fusion_is_idempotent() {
    let a = Scored::new(hit(package_id(1), "axum::routing::get", None), score(0.9));
    let b = Scored::new(
        hit(
            package_id(1),
            "axum::routing::get",
            Some(reference_for("axum", INTRO_A)),
        ),
        score(0.2),
    );

    let once = Symbols::fuse(a, b);
    let twice = Symbols::fuse(once.clone(), once.clone());
    assert_eq!(once, twice);
}

// ---------------------------------------------------------------------------
// 3. The wire
// ---------------------------------------------------------------------------

/// The reference must survive the wire — it is the whole reason the hit crosses
/// it, once the index can supply one.
#[test]
fn a_reference_round_trips_through_json() {
    let navigable = hit(
        package_id(1),
        "axum::routing::get",
        Some(reference_for("axum", INTRO_A)),
    );

    let encoded = serde_json::to_string(&navigable).expect("must serialize");
    let decoded: SymbolHit = serde_json::from_str(&encoded).expect("must deserialize");
    assert_eq!(decoded, navigable);
}

/// `/search` is the hottest path in the system, and today **every** remote row
/// has no reference. A `"reference": null` per line is overhead paid forever
/// for a value that is absent more often than present — exactly the argument
/// `signature` already makes for itself.
#[test]
fn an_absent_reference_is_omitted_from_the_wire() {
    let bare = hit(package_id(1), "axum::routing::get", None);
    let encoded = serde_json::to_string(&bare).expect("must serialize");
    assert!(
        !encoded.contains("reference"),
        "an absent reference must not appear on the wire: {encoded}"
    );
}

/// A hit encoded before this field existed must still decode — the index and
/// the GUI are deployed separately, so an older peer's rows must not become
/// unreadable.
#[test]
fn a_hit_encoded_without_a_reference_still_decodes() {
    let legacy = r#"{
        "package": "00000000-0000-0000-0000-000000000000",
        "path": "axum::routing::get",
        "display_name": "get",
        "ecosystem": "rust",
        "kind": "Function"
    }"#;

    let decoded: SymbolHit =
        serde_json::from_str(legacy).expect("an older peer's row must still decode");
    assert!(decoded.reference.is_none());
}

/// The reference must serialize as the frozen grammar, not an ad-hoc
/// re-encoding: the index parses this string back with
/// `StableReference::parse` for `Target::Usages`, so drift here silently breaks
/// the usages route.
#[test]
fn a_reference_serializes_as_the_frozen_grammar() {
    let reference = reference_for("axum", INTRO_A);
    let encoded = serde_json::to_string(&reference).expect("must serialize");
    let raw = encoded.trim_matches('"');

    assert_eq!(raw, format!("F:rust/axum#{INTRO_A}"));
    assert_eq!(
        StableReference::parse(raw).expect("must round-trip through the parser"),
        reference
    );
}
