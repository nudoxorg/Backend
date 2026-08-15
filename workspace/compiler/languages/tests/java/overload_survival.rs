//! Gson's `fromJson` overloads must survive sealing as distinct declarations.
//!
//! # The defect this pins (docs/LIMITATIONS.md L39)
//!
//! 28 real `Function` entries — ~4.3% of Gson's 655 — vanished between
//! `Lowering::finish` and the sealed table, silently.
//!
//! Two mechanisms compounded:
//!
//! 1. `lower_type` returned `Type::Any` for every type outside the extraction.
//!    `Skeleton` encodes `Type::Any` as the single byte `0x09`, so
//!    `fromJson(String, Class)`, `(String, Type)`, `(Reader, Class)` and
//!    `(Reader, Type)` all encoded to `[Any, Any] -> T` — four byte-identical
//!    signature skeletons.
//! 2. `seal`'s `Kind::Function` branch took `Disambiguator::FnOverload(skeleton)`
//!    whenever the base key collided, with **no fallthrough** when the skeleton
//!    turned out to carry no discriminating bytes — while every *non*-Function
//!    collision did fall through to `Disambiguator::Span`. Four identical
//!    skeletons therefore minted one `IntroId`, and
//!    `PristineIntroTable::insert_live` was a bare `HashMap::insert` whose
//!    displaced entry went on the floor with no return channel.
//!
//! `JavaId::method` had distinguished all four all along — `type_erase` puts
//! the full FQN of every parameter in the id. Seal discarded it.
//!
//! # Ground truth
//!
//! `tests/java/fixtures/gson/com/google/gson/Gson.java` declares eleven `fromJson`
//! overloads (lines 1106, 1136, 1166, 1197, 1229, 1259, 1304, 1345, 1405, 1433,
//! 1459), including all four named in the defect report:
//! `(String, Class)`, `(String, Type)`, `(Reader, Class)`, `(Reader, Type)`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use nudox_ir::{
    change::{EcosystemId, PackageLineageId, PackageName},
    index::Ref,
    kind::Kind,
    kinds::ty::Type,
};
use nudox_languages::{PackageSource, produce};
use nudox_languages::java::JavaProducer;

fn fixture_root(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/java/fixtures")
        .join(name)
}

fn error_chain(err: &dyn std::error::Error) -> String {
    let mut chain = err.to_string();
    let mut cursor: &dyn std::error::Error = err;
    while let Some(source) = std::error::Error::source(cursor) {
        let _ = write!(chain, "\n  caused by: {source}");
        cursor = source;
    }
    chain
}

/// The eleven `Gson.fromJson` overloads must all be present, with eleven
/// distinct identities and eleven distinct parameter-type lists.
///
/// Asserts on the parameter types each overload actually carries, not on a
/// count: a count of eleven is satisfied by eleven entries that all read
/// `(Any, Any) -> T`, which is precisely the state that produced the collision.
#[test]
fn gson_fromjson_overloads_survive_seal_as_distinct_declarations() {
    let root = fixture_root("gson");
    let src = PackageSource::new(&root, "gson", "2.11.0");
    let lineage = PackageLineageId::new(EcosystemId::new("maven"), PackageName::new("gson"));

    let (produced, cost) = heart::cost::measured("l39/gson-2.11.0/overloads", &root, || {
        produce(&JavaProducer::new(), &src, &lineage, &nudox_ir::foreign::Unlinked)
            .unwrap_or_else(|e| panic!("gson must lower without error:\n{}", error_chain(&e)))
    });

    let table = &produced.table;
    let report = &produced.report;
    let recovered: usize = report
        .forced
        .iter()
        .map(|f| f.group.saturating_sub(1))
        .sum();
    eprintln!(
        "gson sealed {} entries in {:.2}s — {} unlinked foreign keys, \
         {} forced groups recovering {} declarations, {} lost collisions",
        table.len(),
        cost.wall.as_secs_f64(),
        report.unlinked.len(),
        report.forced.len(),
        recovered,
        report.collisions.len(),
    );

    assert!(
        report.collisions.is_empty(),
        "declarations were still lost after escalation: {:?}",
        report
            .collisions
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
    );
    assert!(
        report.unmapped_local.is_empty(),
        "arena-local refs survived seal: {:?}",
        report.unmapped_local
    );

    // Every `fromJson` whose owning class is `Gson`.
    let mut sigs: Vec<(nudox_ir::change::IntroId, Vec<String>)> = Vec::new();
    for (intro, entry) in table.iter() {
        if entry.sym().name != "fromJson" {
            continue;
        }
        let Some(parent) = table.parent_of(intro) else {
            continue;
        };
        if table.get(parent).map(|p| p.sym().name.as_str()) != Some("Gson") {
            continue;
        }
        let Some(Kind::Function(f)) = entry.kind().as_owned_kind() else {
            continue;
        };

        // Render each parameter's type by name. A foreign type must name
        // itself; `Type::Any` here means the erasure is back.
        let mut params: Vec<String> = Vec::new();
        for pref in &f.input_params {
            let Ref::Intro(pid) = pref else {
                panic!("a param ref must be same-package and sealed, got {pref:?}");
            };
            let pe = table.get(*pid).expect("param entry must be live");
            let Some(Kind::Param(p)) = pe.kind().as_owned_kind() else {
                panic!("expected a Param entry");
            };
            params.push(type_name(p.ty.as_ref(), table));
        }
        sigs.push((intro, params));
    }

    assert_eq!(
        sigs.len(),
        11,
        "Gson.java declares eleven `fromJson` overloads (lines 1106, 1136, 1166, \
         1197, 1229, 1259, 1304, 1345, 1405, 1433, 1459); {} survived seal",
        sigs.len()
    );

    let mut intros: Vec<_> = sigs.iter().map(|(i, _)| *i).collect();
    intros.sort();
    intros.dedup();
    assert_eq!(
        intros.len(),
        11,
        "the eleven overloads must hold eleven distinct IntroIds; \
         a shared id means one silently overwrote another"
    );

    // The four named in the defect report, by their first parameter and the
    // shape of their second.
    let rendered: Vec<String> = sigs
        .iter()
        .map(|(_, p)| format!("fromJson({})", p.join(", ")))
        .collect();
    let mut sorted = rendered.clone();
    sorted.sort();
    eprintln!("--- Gson.fromJson overloads ---");
    for s in &sorted {
        eprintln!("{s}");
    }
    eprintln!("-------------------------------");

    for expected in [
        "fromJson(String, Class)",
        "fromJson(String, Type)",
        "fromJson(Reader, Class)",
        "fromJson(Reader, Type)",
    ] {
        assert!(
            rendered.iter().any(|r| r == expected),
            "`{expected}` must survive seal with its real parameter types; got {sorted:?}"
        );
    }

    // No overload may have had its parameters erased: that erasure is what made
    // the skeletons identical in the first place.
    assert!(
        !rendered.iter().any(|r| r.contains("any")),
        "a parameter erased to `Type::Any` is the root cause of the collision; \
         got {sorted:?}"
    );

    // Distinct signatures, not merely distinct ids.
    let mut uniq = rendered;
    uniq.sort();
    uniq.dedup();
    assert_eq!(
        uniq.len(),
        11,
        "eleven overloads must render eleven distinct parameter lists; got {sorted:?}"
    );
}

/// A parameter type's display name — the leaf a reader sees.
///
/// Same-package types resolve through the table; cross-package ones carry their
/// own display name on the key, because nothing downstream has a corpus to look
/// them up in.
fn type_name(ty: Option<&Type>, table: &nudox_ir::apply::PristineIntroTable) -> String {
    match ty {
        None => "<none>".to_owned(),
        Some(Type::Any) => "any".to_owned(),
        Some(Type::Apply { base, .. }) => type_name(Some(base), table),
        Some(Type::Nominal(Ref::Foreign { key, .. })) => key.display.to_string(),
        Some(Type::Nominal(Ref::Local(_))) => "<dangling>".to_owned(),
        Some(Type::Nominal(Ref::Intro(id))) => table
            .get(*id).map_or_else(|| "<unresolved-intro>".to_owned(), |e| e.sym().name.clone()),
        Some(Type::TypeVar(n)) => n.clone(),
        Some(other) => format!("{other:?}"),
    }
}
