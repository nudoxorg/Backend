//! The versioned edge-tip walk and a full materialize of the same rows
//! report the same in-degree. A dev edge, a self-edge, a cross-ecosystem
//! name, and a dropped version must not move the other ecosystem's count.

use std::collections::HashMap;

use heart::Language;
use index::{
    engine::turso_vc::VersionedCatalog,
    record::{DepClass, DepEdge, PackageRecord},
    search::ranking::dependents::{DependencyRow, count_dependents},
};
use smol_str::SmolStr;

fn edge(name: &str, class: DepClass) -> DepEdge {
    DepEdge {
        name: SmolStr::new(name),
        requirement: None,
        class,
        kind: match class {
            DepClass::Runtime | DepClass::Optional => index::enums::EdgeKind::Runtime,
            DepClass::Dev | DepClass::Build | DepClass::Peer => index::enums::EdgeKind::Build,
        },
        optional: false,
        dep_ecosystem: None,
    }
}

fn record(ecosystem: Language, name: &str, version: &str, edges: Vec<DepEdge>) -> PackageRecord {
    let mut record = PackageRecord::published(ecosystem, name, version, &[] as &[&str]);
    record.edges = edges;
    record
}

fn materialized_rows(catalog: &mut VersionedCatalog) -> Vec<DependencyRow> {
    let coords = [
        (Language::Rust, "memchr", "2.7.0"),
        (Language::Rust, "memchr", "2.8.0"),
        (Language::Python, "memchr", "1.0.0"),
    ];
    coords
        .into_iter()
        .filter_map(|(ecosystem, name, version)| {
            let record = catalog
                .materialize(ecosystem.as_token(), name, version)
                .expect("read")?;
            let dependencies: Vec<SmolStr> = record
                .edges
                .iter()
                .filter(|edge| edge.class.is_runtime_or_optional())
                .map(|edge| edge.name.clone())
                .collect();
            if dependencies.is_empty() {
                return None;
            }
            Some(DependencyRow {
                ecosystem,
                name: SmolStr::new(name),
                dependencies,
            })
        })
        .collect()
}

#[test]
fn tip_degree_matches_materialized_edges_after_a_drop() {
    let mut catalog = VersionedCatalog::open().expect("catalog");
    let writes = [
        record(Language::Rust, "memchr", "2.7.0", vec![
            edge("libc", DepClass::Runtime),
            edge("regex", DepClass::Runtime),
            edge("memchr", DepClass::Runtime),
            edge("criterion", DepClass::Dev),
        ]),
        record(Language::Rust, "memchr", "2.8.0", vec![
            edge("libc", DepClass::Runtime),
            edge("windows-sys", DepClass::Optional),
        ]),
        record(Language::Python, "memchr", "1.0.0", vec![edge(
            "requests",
            DepClass::Runtime,
        )]),
    ];
    for record in &writes {
        catalog.put_record(record).expect("put");
    }
    let forward = catalog.dependents();
    catalog
        .drop_version("rust", "memchr", "2.7.0")
        .expect("drop");
    let after = catalog.dependents();
    let oracle = count_dependents(materialized_rows(&mut catalog));
    assert_eq!(after, oracle);

    let rust = |name: &str| (Language::Rust, SmolStr::new(name));
    let python = |name: &str| (Language::Python, SmolStr::new(name));
    assert_eq!(forward.get(&rust("regex")).copied(), Some(1));
    assert!(!after.contains_key(&rust("regex")));
    assert_eq!(after.get(&rust("libc")).copied(), Some(1));
    assert_eq!(after.get(&rust("windows-sys")).copied(), Some(1));
    assert_eq!(after.get(&python("requests")).copied(), Some(1));
    assert!(!after.contains_key(&rust("criterion")));
    assert!(!after.contains_key(&rust("memchr")));
    assert!(!after.contains_key(&python("libc")));

    let mut reversed = VersionedCatalog::open().expect("catalog");
    for record in writes.iter().rev() {
        reversed.put_record(record).expect("put");
    }
    reversed
        .drop_version("rust", "memchr", "2.7.0")
        .expect("drop");
    let mut again: HashMap<_, _> = reversed.dependents();
    again.retain(|_, count| *count > 0);
    let mut kept = after.clone();
    kept.retain(|_, count| *count > 0);
    assert_eq!(again, kept);
}
