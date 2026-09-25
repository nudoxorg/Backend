use super::*;
use uuid::Uuid;

fn pid(n: u128) -> PackageId {
    PackageId::from_uuid(Uuid::from_u128(n))
}

fn item(n: u128, name: &str, score: f32) -> RankedItem {
    RankedItem {
        id: pid(n),
        name: name.to_string(),
        score,
    }
}

fn ids(hits: &[EnrichedHit]) -> Vec<Option<PackageId>> {
    hits.iter().map(|h| h.package_id).collect()
}

fn registry_ids(hits: &[EnrichedHit]) -> Vec<PackageId> {
    hits.iter().filter_map(|h| h.package_id).collect()
}

/// 1. Empty LocalContext → order identical, labels empty / None.
#[test]
fn empty_context_is_identity() {
    let ranked = vec![
        item(1, "alpha", 10.0),
        item(2, "beta", 9.0),
        item(3, "gamma", 8.0),
        item(4, "delta", 7.0),
    ];
    let enrich = LocalEnrichment::default();
    let out = enrich.apply(&ranked, "alpha", &LocalContext::default());

    assert_eq!(out.len(), ranked.len());
    for (i, hit) in out.iter().enumerate() {
        assert_eq!(hit.package_id, Some(ranked[i].id));
        assert_eq!(hit.name, ranked[i].name);
        assert_eq!(hit.score.to_bits(), ranked[i].score.to_bits());
        assert_eq!(hit.source, HitSource::Registry);
        assert!(hit.dep_relation.is_none());
        assert!(hit.usage.is_none());
        assert!(hit.labels.is_empty());
        assert!(hit.local_only.is_none());
    }
}

/// 2. Used-before can rise a few slots but cannot beat a much higher
///    exact-quality head item when bonuses are capped.
#[test]
fn used_before_climbs_but_cannot_leapfrog_top_exact() {
    // Dense scores just below a decisive head: default max_additive_bonus
    // (0.9) lifts the used package past mid-pack peers but not past head
    // (10.0). max_rank_climb further bounds position even if bonuses were wild.
    let ranked = vec![
        item(1, "serde", 10.0), // exact high-quality head
        item(2, "serde-json", 9.1),
        item(3, "serde-yaml", 9.05),
        item(4, "serde-bytes", 9.02),
        item(5, "other", 9.01),
        item(6, "used-pkg", 9.0), // used before — may climb a little
    ];

    let mut ctx = LocalContext::default();
    ctx.usage.insert(pid(6), UsageStat {
        times_depended: 12,
        last_used: Some(1_700_000_000),
    });

    let enrich = LocalEnrichment::default();
    let out = enrich.apply(&ranked, "serde", &ctx);

    assert_eq!(
        out[0].package_id,
        Some(pid(1)),
        "used-before must not leapfrog top exact-quality hit"
    );
    assert!(
        out[0].labels.is_empty(),
        "top exact hit has no local usage in this fixture"
    );

    let used_pos = out
        .iter()
        .position(|h| h.package_id == Some(pid(6)))
        .expect("used package still present");
    let original_pos = 5usize;
    assert!(
        used_pos < original_pos,
        "used-before should rise at least one slot when climb allows; pos={used_pos}"
    );
    assert!(
        original_pos - used_pos <= enrich.max_rank_climb,
        "climb must respect max_rank_climb"
    );
    assert!(out[used_pos].labels.contains(&LocalLabel::UsedBefore));

    // Adversarial: even with huge configured bonuses, hard caps hold.
    let wild = LocalEnrichment {
        used_before_bonus: 100.0,
        direct_dep_bonus: 100.0,
        max_additive_bonus: 0.9, // still capped under exact_name scale
        max_rank_climb: 2,
        ..Default::default()
    };
    let out_wild = wild.apply(&ranked, "serde", &ctx);
    assert_eq!(
        out_wild[0].package_id,
        Some(pid(1)),
        "capped bonus must not invert head even if raw knobs are huge"
    );
    let used_pos_wild = out_wild
        .iter()
        .position(|h| h.package_id == Some(pid(6)))
        .unwrap();
    assert!(
        original_pos - used_pos_wild <= wild.max_rank_climb,
        "max_rank_climb hard-stops leapfrogging"
    );
}

/// 3. Local-only name match injects with LocalOnly + LocalFork/PathDep.
#[test]
fn local_only_matching_name_injects() {
    let ranked = vec![item(1, "serde", 10.0), item(2, "tokio", 9.0)];
    let mut ctx = LocalContext::default();
    ctx.local_only.push(LocalOnlyPackage {
        local_id: "path:my-fork".into(),
        ecosystem: Language::Rust,
        name: "my-fork".into(),
        description: Some("local path dep".into()),
        keywords: vec![],
        upstream: None,
    });
    ctx.local_only.push(LocalOnlyPackage {
        local_id: "fork:serde-local".into(),
        ecosystem: Language::Rust,
        name: "serde-local".into(),
        description: None,
        keywords: vec![],
        upstream: Some(pid(1)),
    });

    let enrich = LocalEnrichment::default();
    let out = enrich.apply(&ranked, "my-fork", &ctx);

    let local = out
        .iter()
        .find(|h| h.source == HitSource::LocalOnly)
        .expect("local-only row injected");
    assert_eq!(local.name, "my-fork");
    assert_eq!(local.package_id, None);
    assert!(local.labels.contains(&LocalLabel::PathDep));
    assert!(local.local_only.is_some());

    // Fork (with upstream) gets LocalFork when its name matches.
    let out_fork = enrich.apply(&ranked, "serde-local", &ctx);
    let fork = out_fork
        .iter()
        .find(|h| h.source == HitSource::LocalOnly)
        .expect("fork injected");
    assert!(fork.labels.contains(&LocalLabel::LocalFork));
}

/// 4. Local-only does not appear when name doesn't match the query.
#[test]
fn local_only_non_matching_name_not_injected() {
    let ranked = vec![item(1, "serde", 10.0)];
    let mut ctx = LocalContext::default();
    ctx.local_only.push(LocalOnlyPackage {
        local_id: "path:other".into(),
        ecosystem: Language::Rust,
        name: "totally-unrelated".into(),
        description: None,
        keywords: vec![SmolStr::from("zzz")],
        upstream: None,
    });

    let out = LocalEnrichment::default().apply(&ranked, "serde", &ctx);
    assert!(out.iter().all(|h| h.source == HitSource::Registry));
    assert_eq!(registry_ids(&out), vec![pid(1)]);
}

/// 5. max_local_inject is respected.
#[test]
fn max_local_inject_respected() {
    let ranked = vec![
        item(1, "a", 10.0),
        item(2, "b", 9.0),
        item(3, "c", 8.0),
        item(4, "d", 7.0),
    ];
    let mut ctx = LocalContext::default();
    for i in 0..4 {
        ctx.local_only.push(LocalOnlyPackage {
            local_id: format!("local-{i}"),
            ecosystem: Language::Rust,
            name: format!("widget-{i}"),
            description: None,
            keywords: vec![],
            upstream: None,
        });
    }

    let enrich = LocalEnrichment {
        max_local_inject: 3,
        ..Default::default()
    };
    // Query substring matches all widget-* names.
    let out = enrich.apply(&ranked, "widget", &ctx);
    let local_count = out
        .iter()
        .filter(|h| h.source == HitSource::LocalOnly)
        .count();
    assert_eq!(local_count, 3, "only max_local_inject locals may appear");
}

/// 6. Privacy: LocalEnrichment is separate from the public wire query;
///    serializing the public `heart::query::Query` JSON has no usage / local
///    fields.
#[test]
fn privacy_public_query_json_has_no_local_fields() {
    // Compile-time documentation: LocalContext is a separate type and does
    // not derive Serialize (see type definition above). The public wire type
    // heart::query::Query must never grow usage / dep_relation / local_only
    // fields.
    use heart::query::{
        PageSpecification, Query, QueryMode, RankSpecification, Routing, Scope, Target,
    };

    let q = Query {
        target: Target::Packages,
        text: "serde".into(),
        scope: Scope::default(),
        rank: RankSpecification::default(),
        mode: QueryMode::default(),
        routing: Routing::default(),
        session: None,
        at: None,
        page: PageSpecification::default(),
        query_id: None,
    };
    let json = serde_json::to_value(&q).expect("heart::query::Query serializes");
    let obj = json.as_object().expect("object");

    for forbidden in [
        "usage",
        "dep_relation",
        "local_only",
        "local_context",
        "times_depended",
        "last_used",
        "used_before",
    ] {
        assert!(
            !obj.contains_key(forbidden),
            "heart::query::Query JSON must not contain privacy field {forbidden:?}: {obj:?}"
        );
    }

    // LocalEnrichment / LocalContext are distinct types from heart::query::Query.
    let _enrich = LocalEnrichment::default();
    let _ctx = LocalContext::default();
    assert_ne!(
        std::any::type_name::<LocalContext>(),
        std::any::type_name::<Query>()
    );
    assert_ne!(
        std::any::type_name::<LocalEnrichment>(),
        std::any::type_name::<Query>()
    );

    // Keys that *are* on the wire stay only the public query surface.
    use std::collections::HashSet;
    let keys: HashSet<&str> = obj.keys().map(String::as_str).collect();
    assert!(keys.contains("text"));
    assert!(keys.contains("target"));
}

/// 7. Transitive vs Direct labels differ (and only Direct gets a score lift).
#[test]
fn transitive_vs_direct_labels_differ() {
    let ranked = vec![
        item(1, "head", 10.0),
        item(2, "direct-pkg", 8.0),
        item(3, "trans-pkg", 8.0),
    ];
    let mut ctx = LocalContext::default();
    ctx.dep_relation.insert(pid(2), DepRelation::Direct);
    ctx.dep_relation.insert(pid(3), DepRelation::Transitive);

    let enrich = LocalEnrichment::default();
    let out = enrich.apply(&ranked, "pkg", &ctx);

    let direct = out
        .iter()
        .find(|h| h.package_id == Some(pid(2)))
        .expect("direct");
    let trans = out
        .iter()
        .find(|h| h.package_id == Some(pid(3)))
        .expect("transitive");

    assert!(direct.labels.contains(&LocalLabel::DirectDep));
    assert!(!direct.labels.contains(&LocalLabel::TransitiveDep));
    assert!(trans.labels.contains(&LocalLabel::TransitiveDep));
    assert!(!trans.labels.contains(&LocalLabel::DirectDep));

    assert_eq!(direct.dep_relation, Some(DepRelation::Direct));
    assert_eq!(trans.dep_relation, Some(DepRelation::Transitive));

    // Same base score: direct's bonus lifts it above transitive.
    assert!(
        direct.score > trans.score,
        "direct dep bonus should lift score; direct={} trans={}",
        direct.score,
        trans.score
    );
}

#[test]
fn dev_dep_label_and_smaller_bonus_than_direct() {
    let ranked = vec![item(1, "a", 5.0), item(2, "b", 5.0)];
    let mut ctx = LocalContext::default();
    ctx.dep_relation.insert(pid(1), DepRelation::Direct);
    ctx.dep_relation.insert(pid(2), DepRelation::Dev);

    let out = LocalEnrichment::default().apply(&ranked, "x", &ctx);
    let d = out.iter().find(|h| h.package_id == Some(pid(1))).unwrap();
    let v = out.iter().find(|h| h.package_id == Some(pid(2))).unwrap();
    assert!(d.labels.contains(&LocalLabel::DirectDep));
    assert!(v.labels.contains(&LocalLabel::DevDep));
    assert!(d.score > v.score);
}

#[test]
fn additive_bonus_is_hard_capped() {
    let ranked = vec![item(1, "only", 1.0)];
    let mut ctx = LocalContext::default();
    ctx.usage.insert(pid(1), UsageStat {
        times_depended: 99,
        last_used: None,
    });
    ctx.dep_relation.insert(pid(1), DepRelation::Direct);

    let enrich = LocalEnrichment {
        used_before_bonus: 5.0,
        direct_dep_bonus: 5.0,
        max_additive_bonus: 0.75,
        ..Default::default()
    };

    let out = enrich.apply(&ranked, "only", &ctx);
    assert_eq!(out.len(), 1);
    assert!((out[0].score - (1.0 + 0.75)).abs() < 1e-5);
}

/// Silence dead-code style warning if helper is unused in future edits.
#[test]
fn ids_helper_smoke() {
    let ranked = vec![item(1, "a", 1.0)];
    let out = LocalEnrichment::default().apply(&ranked, "a", &LocalContext::default());
    assert_eq!(ids(&out), vec![Some(pid(1))]);
}
