//! CC-2 census — measure the type lattice against **all 22 provisioned pypi
//! packages**, not against constructed values.
//!
//! # What this file is for
//!
//! Two things that unit tests structurally cannot do.
//!
//! **1. Prove the split is load-bearing at corpus scale.** A hand-written test
//! can assert that `Unannotated` and `DynamicallyTyped` are different values;
//! only a real sweep can show that both actually *occur*, in quantity, across
//! third-party code. The census that motivated CC-2 measured 87,101 annotation
//! positions across these same packages:
//!
//! | fact | positions | share |
//! |---|---:|---:|
//! | unannotated (`ty: None`) | 19,741 | 22.7% |
//! | explicitly dynamic (`typing.Any`, …) | 7,586 | 8.7% |
//! | cross-package nominal, unresolved | 20,227 | 23.2% |
//! | structured | 39,531 | 45.4% |
//!
//! 41.3% of annotated positions collapsed onto one `Type::Any` opcode, and the
//! unannotated case could not be spelled at all. This test asserts that each of
//! those facts now has a distinct, countable representation.
//!
//! **2. Prove the renderer survives real data.** `nudox-graph`'s `Field.typeStr`
//! was `format!("{t:?}")` — a `Debug` dump shipped to MCP clients. The
//! replacement (`nudox_ir::render`) is exercised here against every type in
//! every sealed table, because a renderer that works on ten constructed values
//! and produces an empty string on the eleven-thousandth real one is not a
//! renderer.
//!
//! # Running
//!
//! ```text
//! cargo test -p nudox-languages --test python_type_lattice_census -- --nocapture
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use nudox_ir::{
    change::{EcosystemId, PackageLineageId, PackageName},
    entry::EntryInner,
    kind::Kind,
    kinds::{
        Type, UnknownType,
        ty::{Primitive, TupleElement},
    },
};
use nudox_languages::python::PythonProducer;
use nudox_languages::{PackageSource, produce};

/// The 22 provisioned pypi entries, mirroring `corpus_sweep.rs::ENTRIES`.
/// Kept as `(dir, name, version)` because this test needs nothing else.
const ENTRIES: &[(&str, &str, &str)] = &[
    ("requests-2.31.0", "requests", "2.31.0"),
    ("click-8.0.4", "click", "8.0.4"),
    ("click-8.1.7", "click", "8.1.7"),
    ("pydantic-1.10.14", "pydantic", "1.10.14"),
    ("pydantic-2.6.1", "pydantic", "2.6.1"),
    ("flask-3.0.2", "flask", "3.0.2"),
    ("attrs-23.2.0", "attrs", "23.2.0"),
    ("sqlalchemy-2.0.27", "sqlalchemy", "2.0.27"),
    ("pyyaml-6.0.1", "pyyaml", "6.0.1"),
    ("python-dateutil-2.8.2", "python-dateutil", "2.8.2"),
    ("six-1.16.0", "six", "1.16.0"),
    ("rich-13.7.0", "rich", "13.7.0"),
    ("typer-0.9.0", "typer", "0.9.0"),
    ("httpx-0.27.0", "httpx", "0.27.0"),
    ("black-24.2.0", "black", "24.2.0"),
    ("more-itertools-10.2.0", "more-itertools", "10.2.0"),
    ("tenacity-8.2.3", "tenacity", "8.2.3"),
    ("dataclasses-json-0.6.4", "dataclasses-json", "0.6.4"),
    ("structlog-24.1.0", "structlog", "24.1.0"),
    ("jsonschema-4.21.1", "jsonschema", "4.21.1"),
    ("cattrs-23.2.3", "cattrs", "23.2.3"),
    ("beautifulsoup4-4.12.3", "beautifulsoup4", "4.12.3"),
];

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../result")
        .canonicalize()
        .expect("no result/ checkout — see docs/CORPUS.md to (re)provision it")
}

/// What one package contributed to the census.
#[derive(Default)]
struct Census {
    /// Every type position seen, at every nesting depth.
    positions: usize,
    /// `Type::Any` — must be zero for Python, which has no top type.
    top_type: usize,
    /// Reason tag → count.
    reasons: BTreeMap<&'static str, usize>,
    /// Distinct spellings recovered by the name-carrying reasons.
    local_names: usize,
    external_names: usize,
    /// Renderings that came out empty or leaked `Debug` syntax.
    bad_renderings: Vec<String>,
}

impl Census {
    fn merge(&mut self, other: Census) {
        self.positions += other.positions;
        self.top_type += other.top_type;
        for (k, v) in other.reasons {
            *self.reasons.entry(k).or_default() += v;
        }
        self.local_names += other.local_names;
        self.external_names += other.external_names;
        self.bad_renderings.extend(other.bad_renderings);
    }

    fn count(&self, tag: &str) -> usize {
        self.reasons.get(tag).copied().unwrap_or(0)
    }
}

/// Record one type position, then recurse into it.
///
/// Recursion is the point: an `Apply { base, args: [Unknown(..)] }` carries the
/// gap in its arguments, which is exactly where a generic's real type lives. A
/// top-level-only census would report those positions as `Apply` and miss every
/// erased argument.
fn visit(ty: &Type, c: &mut Census) {
    c.positions += 1;

    let rendered = ty.to_string();
    if rendered.is_empty()
        || rendered.contains("Primitive(")
        || rendered.contains("Unknown(")
        || rendered.contains("Fixed(")
        || rendered.contains("{ signed")
    {
        // Cap the report: one page of examples diagnoses the bug, 30,000 does
        // not.
        if c.bad_renderings.len() < 20 {
            c.bad_renderings.push(format!("{ty:?} -> {rendered:?}"));
        }
    }

    match ty {
        Type::Any => c.top_type += 1,
        Type::Unknown(r) => {
            *c.reasons.entry(r.tag()).or_default() += 1;
            match r {
                UnknownType::UnresolvedLocalName { .. } => c.local_names += 1,
                UnknownType::UnresolvedExternal { .. } => c.external_names += 1,
                _ => {}
            }
        }
        _ => {}
    }

    match ty {
        Type::Slice(t) | Type::Array { ty: t, .. } => visit(t, c),
        Type::Union(ts) | Type::Intersection(ts) | Type::ImplTrait(ts) | Type::DynTrait(ts) => {
            for t in ts {
                visit(t, c);
            }
        }
        Type::Apply { base, args } => {
            visit(base, c);
            for a in args {
                visit(a, c);
            }
        }
        Type::Tuple(elems) => {
            for e in elems {
                match e {
                    TupleElement::Positional(t) => visit(t, c),
                    TupleElement::Named { ty, .. } => visit(ty, c),
                }
            }
        }
        Type::Annotated { inner, .. } => visit(inner, c),
        Type::FunctionPointer { params, ret, .. } => {
            for p in params {
                visit(p, c);
            }
            if let Some(r) = ret {
                visit(r, c);
            }
        }
        Type::Wildcard { bound: Some(b), .. } => visit(b, c),
        Type::QualifiedPath {
            self_ty, trait_ref, ..
        } => {
            visit(self_ty, c);
            if let Some(t) = trait_ref {
                visit(t, c);
            }
        }
        Type::Primitive(p) => match p {
            Primitive::MutPointer(t) | Primitive::ConstPointer(t) => visit(t, c),
            Primitive::Reference { ty, .. } => visit(ty, c),
            _ => {}
        },
        _ => {}
    }
}

fn census_for(dir: &str, name: &str, version: &str) -> Option<(usize, Census)> {
    let root = corpus_root().join(dir);
    if !root.join("setup.py").is_file() && !root.join("pyproject.toml").is_file() {
        return None;
    }
    let src = PackageSource::new(&root, name, version);
    let lid = PackageLineageId::new(EcosystemId::new("pypi"), PackageName::new(name));
    let produced = produce(&PythonProducer, &src, &lid, &nudox_ir::foreign::Unlinked).ok()?;

    let mut c = Census::default();
    for (_, entry) in produced.table.iter() {
        let EntryInner::Owned(kind) = entry.kind() else {
            continue;
        };
        match kind {
            Kind::Param(p) => {
                if let Some(t) = &p.ty {
                    visit(t, &mut c);
                }
            }
            Kind::Field(f) => {
                if let Some(t) = &f.ty {
                    visit(t, &mut c);
                }
            }
            Kind::Const(k) => visit(&k.ty, &mut c),
            Kind::Static(s) => visit(&s.ty, &mut c),
            Kind::Alias(a) => {
                if let Some(t) = &a.target {
                    visit(t, &mut c);
                }
            }
            Kind::Record(r) => {
                for s in &r.super_types {
                    visit(s, &mut c);
                }
            }
            Kind::Trait(t) => {
                for s in &t.supers {
                    visit(s, &mut c);
                }
            }
            _ => {}
        }
    }
    Some((produced.table.len(), c))
}

/// **The corpus-scale assertion.** Every fact CC-2 split apart must be
/// separately observable in real third-party Python.
#[test]
fn the_type_lattice_separates_four_real_facts_across_the_pypi_corpus() {
    let mut total = Census::default();
    let mut packages = 0usize;
    let mut entries = 0usize;

    for (dir, name, version) in ENTRIES {
        let Some((table_len, c)) = census_for(dir, name, version) else {
            eprintln!("SKIP {dir}: not provisioned or did not produce");
            continue;
        };
        eprintln!(
            "{dir}: entries={table_len} type_positions={} reasons={:?}",
            c.positions, c.reasons
        );
        packages += 1;
        entries += table_len;
        total.merge(c);
    }

    assert!(
        packages >= 20,
        "the census needs the provisioned corpus; only {packages} package(s) produced"
    );
    eprintln!(
        "\nCORPUS TOTAL: packages={packages} entries={entries} type_positions={} \
         top_type={} reasons={:?}",
        total.positions, total.top_type, total.reasons
    );

    // --- 1. Python has no top type, so `Type::Any` must never appear. -------
    //
    // `typing.Any` is bidirectionally consistent with every type; `object` is
    // a real nominal class. Neither is the lattice's top. Before CC-2 every
    // one of the ~27,800 positions below was `Type::Any`.
    assert_eq!(
        total.top_type, 0,
        "Python has no top type — `Type::Any` must never be emitted by this producer"
    );

    // --- 2. Each split-out fact must actually occur. ------------------------
    //
    // A test that only checked "no Type::Any" would pass on a producer that
    // answered `OracleGap` everywhere. These four counts are what make the
    // split load-bearing rather than cosmetic.
    let unannotated = total.count("unannotated");
    let dynamic = total.count("dynamically-typed");
    let local = total.count("unresolved-local-name");
    let external = total.count("unresolved-external");

    assert!(
        unannotated > 1_000,
        "unannotated positions were 22.7% of the measured census and were previously \
         *unrepresentable*; got {unannotated}"
    );
    assert!(
        dynamic > 100,
        "explicit `typing.Any` was 8.7% of the measured census; got {dynamic}"
    );
    assert!(
        external > 1_000,
        "cross-package nominals were 23.2% of the measured census; got {external}"
    );
    assert!(
        local > 500,
        "13,724 of 20,227 unresolved nominals (67.8%) were bare names the package itself \
         declares — if this is near zero the suffix index is not working and two thirds of \
         the work is being sent to the wrong pass; got {local}"
    );

    // --- 3. And they must be genuinely separate, not one reason wearing four
    // hats. Four distinct tags with substantial counts each.
    let substantial = total.reasons.values().filter(|n| **n > 100).count();
    assert!(
        substantial >= 4,
        "at least four reasons must carry real volume, or `Type::Any` has merely been \
         renamed; got {:?}",
        total.reasons
    );

    // --- 4. The renderer survives every real type. --------------------------
    assert!(
        total.bad_renderings.is_empty(),
        "the renderer produced empty output or leaked `Debug` syntax on real corpus data \
         ({} example(s) shown): {:#?}",
        total.bad_renderings.len(),
        total.bad_renderings
    );
    assert!(
        total.positions > 50_000,
        "the sweep should see tens of thousands of type positions; got {}",
        total.positions
    );
}
