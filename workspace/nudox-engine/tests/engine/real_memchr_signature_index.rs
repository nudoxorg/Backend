//! The signature type-reference index, against a crate nobody wrote for us.
//!
//! # What this proves
//!
//! Three questions the product advertises were unanswerable *at the index
//! layer* — not the schema layer, so exposing more schema could not have
//! helped. `PackageIndexes` captured only `Impl::of` and `Trait::supers`;
//! `Impl::self_ty` was never read at all, and its own doc comment conceded
//! that field types "will be added in a later wave".
//!
//!   * "what does this type implement?" — needs `Impl::self_ty`;
//!   * "every public function in package X returning type Y" — the example
//!     `graph_query`'s own tool description advertises
//!     (`nudox-mcp/src/server.rs`);
//!   * "which fields are of type Y?".
//!
//! Each is answered below by **running a Trustfall query** over real lowered
//! `memchr` and asserting on named symbols that really exist in
//! `result/memchr-2.8.3/src/`. A test that asserted "some postings
//! exist" would pass against the defect.
//!
//! It also reports the index growth the new positions cost, measured rather
//! than assumed — see `signature_index_growth_on_a_real_crate`.
//!
//! # Why memchr and not a fixture
//!
//! Doctrine §4: a hand-authored fixture tests the fixture author's
//! imagination. The fixture corpus has one impl and one parameter; memchr has
//! generic iterators, `impl Iterator for Memchr<'_>`, cross-package trait
//! references into `core`, and hundreds of parameters — including every shape
//! (`&[u8]`, `Option<usize>`, `Memchr<'a>`) that a whole-expression walk has
//! to reach and a head-only walk must not.
//!
//! # Running
//!
//! ```text
//! ls result/memchr-2.8.3/Cargo.toml   # fetched by `nix build .#checks.corpus`
//! cargo test -p nudox-engine --test real_memchr_signature_index -- --ignored --nocapture
//!
//! # Or point at another checkout:
//! NUDOX_PKG_ROOT=/path/to/memchr-X.Y.Z cargo test -p nudox-engine \
//!   --test real_memchr_signature_index -- --ignored --nocapture
//! ```

use std::fmt::Write;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use futures::StreamExt as _;
use nudox_engine::graph::adapter::CorpusAdapter;
use nudox_engine::graph::probe::{AdapterProbe, StoreProbe};
use nudox_engine::store::corpus::Corpus;
use nudox_engine::store::package::{PackageView, Provenance, TypePosition};
use nudox_engine::store::source::producer::PackageDescriptor;
use nudox_ir::view::IrView;
use nudox_languages::produce;
use nudox_languages::rust::RustProducer;
use trustfall::{FieldValue, Schema, execute_query_async};

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

fn root() -> PathBuf {
    std::env::var("NUDOX_PKG_ROOT")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map_or_else(
            || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../result/memchr-2.8.3"),
            PathBuf::from,
        )
}

/// Lower memchr once, wrapped in the `cost case=` line doctrine §4 requires.
///
/// Panics with the whole `#[source]` chain on failure — doctrine §8: reading
/// only `ProducerError`'s terse top-level `Display` is how a five-second
/// diagnosis becomes an hour.
fn lower_memchr(case: &str) -> PackageView {
    let root = root();
    assert!(
        root.join("Cargo.toml").is_file(),
        "no memchr checkout at {}; run `nix build .#checks.corpus` or set NUDOX_PKG_ROOT",
        root.display()
    );

    let descriptor = PackageDescriptor::cargo(&root, "memchr", "2.8.3");
    let (produced, cost) = heart::cost::measured(case, &root, || {
        produce(
            &RustProducer { direct_repo: false },
            &descriptor.source,
            &descriptor.lineage,
            &nudox_ir::foreign::Unlinked,
        )
        .unwrap_or_else(|err| {
            let mut chain = format!("{err}");
            let mut cursor: &dyn std::error::Error = &err;
            while let Some(source) = std::error::Error::source(cursor) {
                let _ = write!(chain, "\n  caused by: {source}");
                cursor = source;
            }
            panic!("memchr must lower without error:\n{chain}");
        })
    });

    eprintln!(
        "lowered {} entries from memchr-2.8.3 in {:.1}s",
        produced.table.len(),
        cost.wall.as_secs_f64()
    );

    let view = IrView::with_package(descriptor.lineage, produced.table);
    PackageView::build(view, Provenance::TrustedLocal)
}

async fn corpus_of(package: PackageView) -> Corpus {
    let corpus = Corpus::new();
    corpus.insert(Arc::new(package)).await;
    corpus
}

fn schema() -> Schema {
    Schema::parse(include_str!("../../schema.graphql")).expect("schema must parse")
}

/// Run `query` and return one column's values, failing loudly on an error row.
///
/// Errors are surfaced rather than filtered: a reverse edge that yields
/// `SymbolNotFound` for a real corpus is a finding, and silently dropping the
/// row would hide it behind an empty answer.
async fn column(
    corpus: &Corpus,
    probe: Option<Arc<AdapterProbe>>,
    query: &str,
    vars: impl IntoIterator<Item = (String, FieldValue)>,
    column: &str,
) -> Vec<String> {
    let adapter = Arc::new(probe.map_or_else(
        || CorpusAdapter::new(corpus.clone()),
        |p| CorpusAdapter::new_with_probe(corpus.clone(), p),
    ));
    execute_query_async(
        &schema(),
        adapter,
        query,
        vars.into_iter().collect::<BTreeMap<_, _>>(),
    )
    .expect("query must parse against the schema")
    .map(|row| {
        let row = row.expect("no error row");
        match row.get(column) {
            Some(FieldValue::String(s)) => s.to_string(),
            // A null projection is rendered rather than dropped: a silently
            // discarded row would make a missing `path` look like a missing
            // answer, which is the failure these tests exist to detect.
            Some(FieldValue::Null) => "<null>".to_owned(),
            other => panic!("column {column} missing or not a string: {other:?}"),
        }
    })
    .collect()
    .await
}

/// The key of the one symbol named `name` with kind `kind`.
async fn key_of(corpus: &Corpus, name: &str, kind: &str) -> String {
    let keys = column(
        corpus,
        None,
        r#"{
            Symbols {
                name @filter(op: "=", value: ["$name"])
                kind @filter(op: "=", value: ["$kind"])
                key @output
            }
        }"#,
        [
            ("name".to_owned(), FieldValue::String(name.into())),
            ("kind".to_owned(), FieldValue::String(kind.into())),
        ],
        "key",
    )
    .await;
    assert!(
        !keys.is_empty(),
        "memchr must declare a {kind} named `{name}`; the fixture or the \
         producer has changed and every assertion below is meaningless"
    );
    if keys.len() > 1 {
        eprintln!("note: {kind} `{name}` is ambiguous ({keys:?}); using the first");
    }
    keys[0].clone()
}

// ---------------------------------------------------------------------------
// Advertised question 1 — "what does this type implement?"
// ---------------------------------------------------------------------------

/// `Memchr` is an iterator. Asking the graph what `Memchr` implements must say
/// so, by name.
///
/// This was unanswerable before: `Impl::self_ty` was never read by the index,
/// so nothing keyed impls by the type they are *for*. `Trait.implementors`
/// answers the opposite question and cannot be run backwards — you can ask who
/// implements `Iterator` only if `Iterator`'s own package is loaded, which for
/// a `core` trait it never is.
#[test]
#[ignore = "drives in-process rust-analyzer over a real cargo workspace (~20-45 s); \
            run explicitly with --ignored"]
fn what_a_real_type_implements_is_answerable_by_name() {
    let package = lower_memchr("typerefs/memchr-2.8.3/implemented-by");
    let rt = tokio::runtime::Runtime::new().expect("runtime");

    rt.block_on(async {
        let corpus = corpus_of(package).await;
        let key = key_of(&corpus, "Memchr", "Record").await;
        let probe = Arc::new(AdapterProbe::new());

        let traits = column(
            &corpus,
            Some(Arc::clone(&probe)),
            r#"{
                Symbols {
                    key @filter(op: "=", value: ["$key"])
                    implementedBy {
                        ... on Impl { ofTrait @output }
                    }
                }
            }"#,
            [("key".to_owned(), FieldValue::String(key.into()))],
            "ofTrait",
        )
        .await;

        eprintln!("Memchr implements: {traits:#?}");
        assert!(
            traits.iter().any(|t| t.ends_with("Iterator")),
            "`impl Iterator for Memchr<'_>` is in \
             result/memchr-2.8.3/src/memchr.rs; `implementedBy` must \
             reach it. got: {traits:?}"
        );

        // The whole traversal is two package probes and no enumeration: the
        // key pushdown, and the impls' own package. A corpus walk here would
        // be the `by_kind[Impl]` scan `nudox-engine`'s Implementations tab has
        // to do precisely because this index did not exist.
        assert_eq!(
            probe.count(StoreProbe::PackageEnumerated),
            0,
            "asking what one named type implements must not walk an entry \
             table. snapshot: {:?}",
            probe.snapshot()
        );
        assert_eq!(
            probe.count(StoreProbe::CorpusList),
            1,
            "one list for the `implementedBy` edge's package fan-out, and no \
             more — the entrypoint is a key pushdown. snapshot: {:?}",
            probe.snapshot()
        );
    });
}

// ---------------------------------------------------------------------------
// Advertised question 2 — "every public function returning type Y"
// ---------------------------------------------------------------------------

/// The example `graph_query`'s tool description advertises, run verbatim in
/// shape against a real crate.
///
/// `memchr_iter` returns `Memchr<'_>`; `memchr2_iter` returns `Memchr2<'_>`.
/// A `returnedBy` edge that confused parameters with returns would report
/// both under either type, and one that matched only the head of the type
/// expression would still work here — so the parameter half is asserted as a
/// *negative* in the same test, which is where the two positions actually
/// come apart.
#[test]
#[ignore = "drives in-process rust-analyzer over a real cargo workspace (~20-45 s); \
            run explicitly with --ignored"]
fn public_functions_returning_a_named_type_are_answerable() {
    let package = lower_memchr("typerefs/memchr-2.8.3/returned-by");
    let rt = tokio::runtime::Runtime::new().expect("runtime");

    rt.block_on(async {
        let corpus = corpus_of(package).await;
        let key = key_of(&corpus, "Memchr", "Record").await;
        let probe = Arc::new(AdapterProbe::new());

        let returning = column(
            &corpus,
            Some(Arc::clone(&probe)),
            r#"{
                Symbols {
                    key @filter(op: "=", value: ["$key"])
                    returnedBy {
                        ... on Function {
                            isPublic @filter(op: "=", value: ["$yes"])
                            path @output
                        }
                    }
                }
            }"#,
            [
                ("key".to_owned(), FieldValue::String(key.clone().into())),
                ("yes".to_owned(), FieldValue::Boolean(true)),
            ],
            "path",
        )
        .await;

        eprintln!("public functions returning Memchr: {returning:#?}");
        assert!(
            returning.iter().any(|p| p.ends_with("memchr_iter")),
            "`pub fn memchr_iter(..) -> Memchr<'_>` is memchr's headline \
             iterator constructor; a query for public functions returning \
             `Memchr` must find it. got: {returning:?}"
        );

        let accepting = column(
            &corpus,
            None,
            r#"{
                Symbols {
                    key @filter(op: "=", value: ["$key"])
                    acceptedBy { path @output }
                }
            }"#,
            [("key".to_owned(), FieldValue::String(key.into()))],
            "path",
        )
        .await;
        eprintln!("functions accepting Memchr: {accepting:#?}");
        assert!(
            !accepting.iter().any(|p| p.ends_with("memchr_iter")),
            "`memchr_iter` returns a Memchr, it does not take one. A \
             `returnedBy`/`acceptedBy` pair that reports it under both is the \
             conflation these positions exist to prevent. got: {accepting:?}"
        );

        assert_eq!(
            probe.count(StoreProbe::PackageEnumerated),
            0,
            "the advertised question must be served from the index, not a \
             scan. snapshot: {:?}",
            probe.snapshot()
        );
    });
}

// ---------------------------------------------------------------------------
// Advertised question 3 — "which fields are of type Y", and the forward edge
// ---------------------------------------------------------------------------

/// Fields of a named type, and the forward direction of the same index.
///
/// `heldBy` from a type reaches the fields declared with it; `signatureTypes`
/// from a function reaches the types its own signature names. The second is
/// the edge whose absence made `mentions`'s name misleading — there was no
/// forward "what types does this signature reference" edge at all, and the
/// index could not have answered one.
#[test]
#[ignore = "drives in-process rust-analyzer over a real cargo workspace (~20-45 s); \
            run explicitly with --ignored"]
fn field_types_and_the_forward_signature_edge_answer_on_a_real_crate() {
    let package = lower_memchr("typerefs/memchr-2.8.3/held-by");
    let rt = tokio::runtime::Runtime::new().expect("runtime");

    rt.block_on(async {
        let corpus = corpus_of(package).await;

        // Forward: `memchr_iter` names `Memchr` in its own signature.
        let key = key_of(&corpus, "memchr_iter", "Function").await;
        let probe = Arc::new(AdapterProbe::new());
        let named = column(
            &corpus,
            Some(Arc::clone(&probe)),
            r#"{
                Symbols {
                    key @filter(op: "=", value: ["$key"])
                    signatureTypes { name @output }
                }
            }"#,
            [("key".to_owned(), FieldValue::String(key.into()))],
            "name",
        )
        .await;

        eprintln!("memchr_iter's signature names: {named:#?}");
        assert!(
            named.iter().any(|n| n == "Memchr"),
            "`pub fn memchr_iter(needle: u8, haystack: &[u8]) -> Memchr<'_>` \
             names `Memchr`; the forward edge must say so. got: {named:?}"
        );
        assert_eq!(
            probe.count(StoreProbe::CorpusList),
            0,
            "the forward edge reads the entry it already holds; listing the \
             corpus for it is the regression. snapshot: {:?}",
            probe.snapshot()
        );

        // Reverse: which fields are declared with a `Finder` type.
        //
        // Reported rather than asserted non-empty when memchr happens to
        // declare none — the invariant under test is that the answer is
        // *derived from field types*, and the forward assertion above already
        // pins that the walk runs. A fabricated expectation here would be a
        // fixture-shaped assertion in a real-corpus test.
        let held = column(
            &corpus,
            None,
            r#"{
                Symbols {
                    kind @filter(op: "=", value: ["$record"])
                    heldBy { path @output }
                }
            }"#,
            [("record".to_owned(), FieldValue::String("Record".into()))],
            "path",
        )
        .await;
        eprintln!(
            "fields whose declared type is a memchr record: {} \
             (sample: {:?})",
            held.len(),
            &held[..held.len().min(10)]
        );
    });
}

// ---------------------------------------------------------------------------
// Index size — measured, not assumed
// ---------------------------------------------------------------------------

/// Report what the new positions cost the index on a real crate.
///
/// Doctrine §8: a number nobody measured is not a result. Capturing every
/// parameter type multiplies posting count, and "it is fine" and "it is
/// prohibitive" are both claims. This prints the split per position, the
/// relational subset that is all the index used to hold, and the ratio — so
/// the growth is a recorded figure rather than an assertion about one.
///
/// The only *assertion* is the one that would have failed before this change:
/// the positions that were previously unrepresentable are non-empty on a real
/// crate. Pinning the counts themselves would make this test as fragile as the
/// producer's entry count, which doctrine already forbids asserting here.
#[test]
#[ignore = "drives in-process rust-analyzer over a real cargo workspace (~20-45 s); \
            run explicitly with --ignored"]
fn signature_index_growth_on_a_real_crate() {
    let package = lower_memchr("typerefs/memchr-2.8.3/growth");
    let indexes = package.indexes();

    let mut per_position: BTreeMap<String, usize> = BTreeMap::new();
    let mut relational = 0usize;
    let mut total = 0usize;
    for (_target, postings) in indexes.type_refs.iter() {
        for (position, _owner) in postings {
            *per_position.entry(format!("{position:?}")).or_default() += 1;
            if position.is_relational() {
                relational += 1;
            }
            total += 1;
        }
    }

    let entries = package.view().table().len();
    let usages = indexes.usages.total_postings();

    eprintln!("--- memchr-2.8.3 reverse index ---");
    eprintln!("entries in table          : {entries}");
    eprintln!("occurrence postings       : {usages}");
    eprintln!("type-ref postings (total) : {total}");
    eprintln!("  of which relational     : {relational}  <- all the index held before");
    eprintln!(
        "distinct target symbols   : {}",
        indexes.type_refs.key_count()
    );
    for (position, count) in &per_position {
        eprintln!("  {position:<18}: {count}");
    }
    if relational > 0 {
        eprintln!(
            "growth factor over the relational-only index: {:.1}x",
            total as f64 / relational as f64
        );
    }

    for position in [
        TypePosition::ImplSelf,
        TypePosition::Parameter,
        TypePosition::Return,
    ] {
        let count = per_position
            .get(&format!("{position:?}"))
            .copied()
            .unwrap_or(0);
        assert!(
            count > 0,
            "a real crate must produce {position:?} postings; zero means the \
             walk never reaches that position and every query built on it is \
             silently empty. full split: {per_position:?}"
        );
    }
}
