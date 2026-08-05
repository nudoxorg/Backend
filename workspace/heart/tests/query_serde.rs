//! Serde golden tests for the one query algebra (INDEX-PLAN IP-2 gate, §9).
//!
//! These pin the *wire = domain* contract: the canonical JSON for each
//! [`Target`] variant, [`Scope`]/[`Routing`] defaulting, [`AsOf`] variants,
//! [`PageSpecification`] default, and the [`StableReference`] roundtrip +
//! rejection table (including the slash-bearing package stems RL-10 requires).

use heart::query::{
    AsOf, CatalogCommitHash, PageSpecification, QualityMode, Query, QueryMode, QueryReach,
    RankSpecification, Routing, Scope, StableReference, StableReferenceError, Target,
    UnixMilliseconds,
};

/// Deserialize a JSON literal into `T`, asserting it parses.
fn from<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> T {
    serde_json::from_value(value).expect("canonical JSON deserializes")
}

/// The minimal wire body — only `target` + `text` — fills every optional field
/// with its serving default.
#[test]
fn query_minimal_body_defaults_every_optional_field() {
    let query: Query = from(serde_json::json!({
        "target": "Packages",
        "text": "async runtime",
    }));

    assert_eq!(query.target, Target::Packages);
    assert_eq!(query.text, "async runtime");
    assert_eq!(query.scope, Scope::default());
    assert_eq!(query.rank, RankSpecification::Fused);
    assert_eq!(query.mode, QueryMode::Precise);
    assert_eq!(query.routing, Routing::default());
    assert!(query.session.is_none());
    assert!(query.at.is_none());
    assert_eq!(query.page, PageSpecification::default());
}

/// `Target::Packages` is an externally-tagged unit variant: the bare string.
#[test]
fn target_packages_canonical_json() {
    assert_eq!(
        serde_json::to_value(Target::Packages).unwrap(),
        serde_json::json!("Packages")
    );
    assert_eq!(
        from::<Target>(serde_json::json!("Packages")),
        Target::Packages
    );
}

/// `Target::Symbols` is the bare string too.
#[test]
fn target_symbols_canonical_json() {
    assert_eq!(
        serde_json::to_value(Target::Symbols).unwrap(),
        serde_json::json!("Symbols")
    );
    assert_eq!(
        from::<Target>(serde_json::json!("Symbols")),
        Target::Symbols
    );
}

/// `Target::Usages` carries a stable reference under the `of` key.
#[test]
fn target_usages_canonical_json() {
    let target = Target::Usages {
        of: StableReference::parse("F:rust/axum#deadbeef").unwrap(),
    };
    let json = serde_json::to_value(&target).unwrap();
    assert_eq!(
        json,
        serde_json::json!({ "Usages": { "of": "F:rust/axum#deadbeef" } })
    );
    assert_eq!(from::<Target>(json), target);
}

/// A withdrawn-inclusive, ecosystem-narrowed scope roundtrips exactly.
#[test]
fn scope_roundtrip() {
    let query: Query = from(serde_json::json!({
        "target": "Symbols",
        "text": "Deserialize",
        "scope": {
            "ecosystems": ["rust"],
            "packages": ["serde"],
            "include_withdrawn": true,
        },
    }));
    assert_eq!(query.scope.ecosystems, vec![heart::Language::Rust]);
    assert_eq!(query.scope.packages, vec!["serde".to_owned()]);
    assert!(query.scope.include_withdrawn);
}

/// `AsOf::Time` carries a unix-ms integer; `AsOf::Commit` a hash string.
#[test]
fn as_of_variants_canonical_json() {
    let time = AsOf::Time(UnixMilliseconds(1_700_000_000_000));
    assert_eq!(
        serde_json::to_value(&time).unwrap(),
        serde_json::json!({ "Time": 1_700_000_000_000i64 }),
    );
    assert_eq!(
        from::<AsOf>(serde_json::json!({ "Time": 1_700_000_000_000i64 })),
        time
    );

    let commit = AsOf::Commit(CatalogCommitHash("abc123".to_owned()));
    assert_eq!(
        serde_json::to_value(&commit).unwrap(),
        serde_json::json!({ "Commit": "abc123" }),
    );
    assert_eq!(
        from::<AsOf>(serde_json::json!({ "Commit": "abc123" })),
        commit
    );
}

/// The default page: limit 30, no cursor, and the cursor key is elided.
#[test]
fn page_specification_default() {
    let page = PageSpecification::default();
    assert_eq!(page.limit, 30);
    assert!(page.cursor.is_none());
    assert_eq!(
        serde_json::to_value(&page).unwrap(),
        serde_json::json!({ "limit": 30 })
    );
}

/// Routing lowercases its enum tokens and elides the empty hot-set.
#[test]
fn routing_canonical_json() {
    let routing: Routing = from(serde_json::json!({ "quality": "deep", "reach": "deps" }));
    assert_eq!(routing.quality, QualityMode::Deep);
    assert_eq!(routing.reach, QueryReach::Deps);
    assert!(routing.hot_packages.is_empty());

    // Default routing serializes to lowercase parity/org with no hot list.
    assert_eq!(
        serde_json::to_value(Routing::default()).unwrap(),
        serde_json::json!({ "quality": "parity", "reach": "org" }),
    );
}

/// `mode` defaults to precise and lowercases.
#[test]
fn query_mode_semantic_opt_in() {
    let query: Query = from(serde_json::json!({
        "target": "Symbols",
        "text": "how do I deserialize json",
        "mode": "semantic",
    }));
    assert_eq!(query.mode, QueryMode::Semantic);
}

/// A stable reference roundtrips through its `F:<eco>/<pkg>#<hex>` string form.
#[test]
fn stable_reference_roundtrip() {
    let raw = "F:rust/axum#0a1b2c3d";
    let reference = StableReference::parse(raw).expect("valid reference parses");
    assert_eq!(reference.ecosystem(), "rust");
    assert_eq!(reference.package(), "axum");
    assert_eq!(reference.intro_hex(), "0a1b2c3d");
    assert_eq!(reference.to_string(), raw);

    let json = serde_json::to_value(&reference).unwrap();
    assert_eq!(json, serde_json::json!(raw));
    assert_eq!(from::<StableReference>(json), reference);
}

/// RL-10: slash-bearing repo-slug package stems MUST parse — `#` is the sole
/// terminator, so every `/` before it belongs to the package stem.
#[test]
fn stable_reference_accepts_slash_bearing_package_stem() {
    let raw = "F:cpp/github.com/curl/curl#deadbeef";
    let reference = StableReference::parse(raw).expect("RL-10 repo-slug stems must parse");
    assert_eq!(reference.ecosystem(), "cpp");
    assert_eq!(reference.package(), "github.com/curl/curl");
    assert_eq!(reference.intro_hex(), "deadbeef");
    assert_eq!(reference.to_string(), raw);
}

/// The rejection table: each malformed input maps to its typed error.
#[test]
fn stable_reference_rejection_table() {
    let cases: &[(&str, StableReferenceError)] = &[
        // Missing the `F:` prefix.
        ("rust/axum#deadbeef", StableReferenceError::MissingPrefix),
        // No `#` terminator at all.
        ("F:rust/axum", StableReferenceError::MissingTerminator),
        // No `/` between ecosystem and package.
        (
            "F:rustaxum#deadbeef",
            StableReferenceError::MissingEcosystemSeparator,
        ),
        // Empty ecosystem component.
        ("F:/axum#deadbeef", StableReferenceError::EmptyComponent),
        // Empty package component.
        ("F:rust/#deadbeef", StableReferenceError::EmptyComponent),
        // Empty intro id.
        ("F:rust/axum#", StableReferenceError::EmptyComponent),
        // Uppercase hex in the intro id.
        ("F:rust/axum#DEADBEEF", StableReferenceError::MalformedIntro),
        // Non-hex character in the intro id.
        ("F:rust/axum#zzzz", StableReferenceError::MalformedIntro),
    ];

    for (raw, expected) in cases {
        let error = StableReference::parse(raw).expect_err("malformed reference must be rejected");
        assert_eq!(
            &error, expected,
            "input {raw:?} should reject as {expected:?}"
        );
    }
}

/// A stable reference deserialized from an invalid JSON string surfaces the
/// parse error (the `try_from = "String"` guard).
#[test]
fn stable_reference_deserialize_rejects_malformed() {
    let result: Result<StableReference, _> =
        serde_json::from_value(serde_json::json!("not-a-reference"));
    assert!(
        result.is_err(),
        "a malformed reference string must fail to deserialize"
    );
}

/// A full query with every field set roundtrips byte-for-byte through JSON.
#[test]
fn query_full_roundtrip() {
    let original = Query {
        target: Target::Usages {
            of: StableReference::parse("F:rust/axum#0a1b").unwrap(),
        },
        text: "Router::new".to_owned(),
        scope: Scope {
            ecosystems: vec![heart::Language::Rust],
            packages: vec!["axum".to_owned()],
            include_withdrawn: false,
        },
        rank: RankSpecification::TextOnly,
        mode: QueryMode::Semantic,
        routing: Routing {
            quality: QualityMode::Premium,
            reach: QueryReach::Project,
            hot_packages: vec![heart::PackageId::from_uuid(uuid::Uuid::from_u128(7))],
        },
        session: Some(uuid::Uuid::from_u128(9)),
        at: Some(AsOf::Time(UnixMilliseconds(42))),
        page: PageSpecification {
            limit: 50,
            cursor: Some("opaque".to_owned()),
        },
        query_id: Some(uuid::Uuid::from_u128(11)),
    };

    let json = serde_json::to_value(&original).unwrap();
    let back: Query = serde_json::from_value(json).unwrap();
    assert_eq!(back, original, "a fully-populated query roundtrips exactly");
}

#[test]
fn query_without_query_id_remains_backward_compatible() {
    let query: Query = serde_json::from_value(serde_json::json!({
        "target": "Symbols",
        "text": "Router"
    }))
    .expect("minimal historical query remains valid");
    assert_eq!(query.query_id, None);
}
