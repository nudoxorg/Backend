use super::*;

fn point(package: &str, intro_hex: &str, hash_byte: u8) -> PointId {
    PointId {
        package: SmolStr::new(package),
        intro_hex: SmolStr::new(intro_hex),
        content_hash: [hash_byte; 32],
    }
}

#[test]
fn the_same_symbol_text_skips_a_second_write() {
    let first = symbol_fingerprint("pkg", "intro", "jina", "serde::Deserialize");
    let again = symbol_fingerprint("pkg", "intro", "jina", "serde::Deserialize");
    let renamed = symbol_fingerprint("pkg", "intro", "jina", "serde::Serialize");
    let other_model = symbol_fingerprint("pkg", "intro", "other", "serde::Deserialize");
    assert_eq!(first.content_hash, again.content_hash);
    assert_ne!(first.content_hash, renamed.content_hash);
    assert_ne!(first.content_hash, other_model.content_hash);
    let mut ledger = UpsertLedger::new();
    assert!(ledger.needs_write(&first));
    ledger.commit(&[first.clone()]);
    assert!(!ledger.needs_write(&again));
    assert!(ledger.needs_write(&renamed));
}

#[test]
fn skip_plus_required_equals_next_len_when_prior_is_subset() {
    let kept_a = point("serde", "aa", 1);
    let kept_b = point("serde", "bb", 2);
    let fresh = point("tokio", "cc", 3);
    let prior = vec![kept_b.clone(), kept_a.clone()];
    let next = vec![kept_a.clone(), fresh.clone(), kept_b.clone()];
    assert!(prior.iter().all(|existing| next.contains(existing)));

    let skip = upserts_to_skip(&prior, &next);
    let required = upserts_required(&prior, &next);

    assert_eq!(skip.len() + required.len(), next.len());
    assert_eq!(skip.len(), prior.len());
    assert_eq!(required.len(), next.len() - prior.len());
    assert!(skip.iter().all(|point| !required.contains(point)));
    assert!(skip.iter().any(|point| std::ptr::eq(*point, &next[0])));
    assert!(skip.iter().any(|point| std::ptr::eq(*point, &next[2])));
    assert!(required.iter().any(|point| std::ptr::eq(*point, &next[1])));
}

#[test]
fn changed_content_hash_is_required_not_skipped() {
    let kept = point("serde", "aa", 1);
    let before = point("serde", "bb", 2);
    let rewritten = point("serde", "bb", 9);
    let prior = vec![kept.clone(), before];
    let next = vec![rewritten, kept.clone()];

    let skip = upserts_to_skip(&prior, &next);
    let required = upserts_required(&prior, &next);

    assert_eq!(skip.len() + required.len(), next.len());
    assert_eq!(skip.len(), 1);
    assert_eq!(required.len(), 1);
    assert!(std::ptr::eq(skip[0], &next[1]));
    assert!(std::ptr::eq(required[0], &next[0]));
}

#[test]
fn package_prefix_does_not_collapse_into_intro() {
    let prior = vec![point("ab", "c", 1)];
    let next = vec![point("a", "bc", 1)];
    assert!(upserts_to_skip(&prior, &next).is_empty());
    assert_eq!(upserts_required(&prior, &next).len(), 1);
}

#[test]
fn merge_matches_the_string_tree() {
    let mut prior = Vec::new();
    let mut next = Vec::new();
    for n in 0..200u32 {
        let package = format!("pkg{n:04}");
        let intro = format!("{n:08x}");
        prior.push(PointId {
            package: SmolStr::new(&package),
            intro_hex: SmolStr::new(&intro),
            content_hash: [1; 32],
        });
        if n % 5 != 0 {
            let mut hash = [1u8; 32];
            if n % 7 == 0 {
                hash[0] = 2;
            }
            next.push(PointId {
                package: SmolStr::new(package),
                intro_hex: SmolStr::new(intro),
                content_hash: hash,
            });
        }
    }
    let merge: Vec<_> = upserts_required(&prior, &next)
        .into_iter()
        .map(|point| point.identity())
        .collect();
    let tree: Vec<_> = upserts_required_via_keys(&prior, &next)
        .into_iter()
        .map(|point| point.identity())
        .collect();
    assert_eq!(merge, tree);
}

#[test]
fn ledger_skips_until_the_hash_changes_and_ignores_a_failed_write() {
    let mut ledger = UpsertLedger::new();
    let written = point("serde", "aa", 1);
    assert!(ledger.needs_write(&written));
    assert!(ledger.needs_write(&written));
    ledger.commit(std::slice::from_ref(&written));
    assert!(!ledger.needs_write(&written));
    let rewritten = point("serde", "aa", 2);
    assert!(ledger.needs_write(&rewritten));
}

#[test]
fn merge_beats_the_string_tree_on_a_few_thousand_points() {
    const N: u32 = 4_096;
    let prior: Vec<PointId> = (0..N)
        .map(|n| PointId {
            package: SmolStr::new(format!("package-{n:05}")),
            intro_hex: SmolStr::new(format!("{n:08x}")),
            content_hash: [1; 32],
        })
        .collect();
    let next: Vec<PointId> = (0..N)
        .map(|n| PointId {
            package: SmolStr::new(format!("package-{n:05}")),
            intro_hex: SmolStr::new(format!("{n:08x}")),
            content_hash: if n % 64 == 0 { [2; 32] } else { [1; 32] },
        })
        .collect();
    let loops = 8u32;
    let merge_ns = time(|| {
        for _ in 0..loops {
            std::hint::black_box(upserts_required(&prior, &next));
        }
    });
    let tree_ns = time(|| {
        for _ in 0..loops {
            std::hint::black_box(upserts_required_via_keys(&prior, &next));
        }
    });
    eprintln!(
        "cost case=frontier/vector_project points={N} loops={loops} merge_ns={merge_ns} tree_ns={tree_ns}"
    );
    assert!(
        merge_ns.saturating_mul(2) < tree_ns,
        "merge {merge_ns} ns was not 2× under the string tree {tree_ns} ns"
    );
}

#[test]
fn borrowed_lookup_matches_a_cloning_map_and_beats_it() {
    const N: u32 = 4_096;
    let points: Vec<PointId> = (0..N)
        .map(|n| point(&format!("pkg{n:032}"), &format!("intro{n:032}"), 1))
        .collect();
    let mut ledger = UpsertLedger::new();
    let mut flat: std::collections::HashMap<(SmolStr, SmolStr), [u8; 32]> =
        std::collections::HashMap::new();
    for point in &points {
        ledger.restore(
            point.package.as_str(),
            point.intro_hex.as_str(),
            point.content_hash,
        );
        flat.insert(
            (point.package.clone(), point.intro_hex.clone()),
            point.content_hash,
        );
    }
    let rewritten = point(
        &format!("pkg{:032}", 0u32),
        &format!("intro{:032}", 0u32),
        9,
    );
    let cloning = |point: &PointId| {
        flat.get(&(point.package.clone(), point.intro_hex.clone()))
            .is_none_or(|hash| *hash != point.content_hash)
    };
    assert_eq!(ledger.needs_write(&rewritten), cloning(&rewritten));
    assert!(ledger.needs_write(&rewritten));
    for point in &points {
        assert_eq!(ledger.needs_write(point), cloning(point));
    }
    ledger.forget_package(&format!("pkg{:032}", 1u32));
    assert!(ledger.needs_write(&points[1]));
    assert!(!ledger.needs_write(&points[2]));

    let loops = 8u32;
    let borrow_ns = time(|| {
        for _ in 0..loops {
            for point in &points {
                std::hint::black_box(ledger.needs_write(point));
            }
        }
    });
    let clone_ns = time(|| {
        for _ in 0..loops {
            for point in &points {
                std::hint::black_box(cloning(point));
            }
        }
    });
    eprintln!(
        "cost case=frontier/vector_ledger points={N} loops={loops} borrow_ns={borrow_ns} clone_ns={clone_ns}"
    );
    assert!(
        borrow_ns.saturating_mul(3) < clone_ns.saturating_mul(2),
        "borrowed lookup {borrow_ns} ns was not 1.5× under the cloning map {clone_ns} ns"
    );
}

#[test]
fn raw_ids_match_the_formatted_uuid_and_skip_the_format() {
    const N: u32 = 4_096;
    let model = "jina";
    let text = "serde::Deserialize";
    let hash = content_fingerprint(model, text);
    let mut ledger = UpsertLedger::new();
    let ids: Vec<(uuid::Uuid, uuid::Uuid)> = (0..N)
        .map(|n| {
            (
                uuid::Uuid::from_u128(u128::from(n)),
                uuid::Uuid::from_u128(u128::from(n) + 1),
            )
        })
        .collect();
    for (package, intro) in &ids {
        ledger.restore(&package.to_string(), &intro.to_string(), hash);
        assert!(!ledger.needs_write_ids(package, intro, &hash));
        let formatted = symbol_fingerprint(&package.to_string(), &intro.to_string(), model, text);
        assert!(!ledger.needs_write(&formatted));
    }
    let changed = content_fingerprint(model, "serde::Serialize");
    assert!(ledger.needs_write_ids(&ids[0].0, &ids[0].1, &changed));
    ledger.forget_package(&ids[1].0.to_string());
    assert!(ledger.needs_write_ids(&ids[1].0, &ids[1].1, &hash));
    assert!(!ledger.needs_write_ids(&ids[2].0, &ids[2].1, &hash));

    let loops = 8u32;
    let raw_ns = time(|| {
        for _ in 0..loops {
            for (package, intro) in &ids {
                std::hint::black_box(ledger.needs_write_ids(package, intro, &hash));
            }
        }
    });
    let format_ns = time(|| {
        for _ in 0..loops {
            for (package, intro) in &ids {
                let point =
                    symbol_fingerprint(&package.to_string(), &intro.to_string(), model, text);
                std::hint::black_box(ledger.needs_write(&point));
            }
        }
    });
    eprintln!(
        "cost case=frontier/vector_ids points={N} loops={loops} raw_ns={raw_ns} format_ns={format_ns}"
    );
    assert!(
        raw_ns.saturating_mul(2) < format_ns,
        "raw id lookup {raw_ns} ns was not 2× under formatting {format_ns} ns"
    );
}

fn time(body: impl FnOnce()) -> u128 {
    let start = std::time::Instant::now();
    body();
    start.elapsed().as_nanos()
}
