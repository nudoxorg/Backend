//! A real Rust package through the one producer pipeline.
//!
//! This is deliberately a tiny package written as Rust source and loaded by
//! rust-analyzer, not a hand-assembled `IrView`.  It pins the seam where a
//! frontend's local identities become sealed symbols: declarations, exact
//! source excerpts, and resolved body references must all emerge from the one
//! `produce → Lowering::finish → seal` path.
//!
//! A table count would not catch the historical defect: the old specialised
//! Rust occurrence path could return callers while production `produce` still
//! returned only declarations.  The assertions below identify a real callee
//! and a real caller in source and require the *normal* `Produced` value to
//! join them after sealing.

use std::fs;

use nudox_ir::{
    body::Language,
    change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef},
    foreign::{ForeignKey, ForeignResolver, Resolution},
    kind::KindDiscriminant,
    vocab::{Confidence, ReferenceKind},
};
use nudox_languages::rust::RustProducer;
use nudox_languages::{PackageSource, produce};
use tempfile::TempDir;

const PACKAGE: &str = "nudox_universal_pipeline_fixture";

/// It matters that `caller` has a multi-line body: an excerpt that stops at
/// the signature or returns only the first line is an explicitly observable
/// regression, not merely less helpful formatting.
const LIB_RS: &str = r#"
#[derive(Clone, Debug)]
pub struct Payload(pub u32);

pub fn target(value: Payload) -> Payload {
    Payload(value.0 + 1)
}

pub fn caller(value: Payload) -> Payload {
    // This invocation is the semantic-reference probe.
    target(value)
}

pub fn foreign_caller(value: Payload) {
    std::mem::drop(value);
}
"#;

#[derive(Clone)]
struct ResolveEveryForeign(StableRef);

impl ForeignResolver for ResolveEveryForeign {
    fn resolve(&self, _key: &ForeignKey) -> Resolution {
        Resolution::Resolved(self.0.clone())
    }
}

fn real_fixture() -> TempDir {
    let fixture = tempfile::tempdir().expect("create temporary Rust package");
    fs::create_dir_all(fixture.path().join("src")).expect("create fixture src/");
    fs::write(
        fixture.path().join("Cargo.toml"),
        format!(
            "[package]\nname = \"{PACKAGE}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n"
        ),
    )
    .expect("write fixture manifest");
    let mut source = LIB_RS.to_owned();
    source.push_str("\npub fn many_calls(mut value: Payload) -> Payload {\n");
    for _ in 0..600 {
        source.push_str("    value = target(value);\n");
    }
    source.push_str("    value\n}\n");
    fs::write(fixture.path().join("src/lib.rs"), source).expect("write fixture source");
    fixture
}

#[test]
fn rust_producer_seals_real_source_and_body_references_through_produced() {
    let fixture = real_fixture();
    let source = PackageSource::new(fixture.path(), PACKAGE, "0.1.0");
    let lineage = PackageLineageId::new(
        EcosystemId::new("cargo"),
        PackageName::new(PACKAGE.to_owned()),
    );
    let foreign_target = StableRef::new(
        PackageLineageId::new(
            EcosystemId::new("rust-sysroot"),
            PackageName::new("resolved-test-target"),
        ),
        IntroId::from_raw([0x5a; 32]),
    );
    let resolver = ResolveEveryForeign(foreign_target.clone());
    let (produced, _cost) =
        heart::cost::measured("producer/universal-rust-fixture", fixture.path(), || {
            produce(
                &RustProducer { direct_repo: false },
                &source,
                &lineage,
                &resolver,
            )
        });
    let produced = produced.expect("the self-contained real Rust fixture must lower");

    let target = produced
        .table
        .iter()
        .find(|(_, entry)| {
            entry.sym().name == "target"
                && entry.kind().discriminant() == Some(KindDiscriminant::Function)
        })
        .map(|(intro, _)| intro)
        .expect("the real `pub fn target` declaration must be present");
    let caller = produced
        .table
        .iter()
        .find(|(_, entry)| {
            entry.sym().name == "caller"
                && entry.kind().discriminant() == Some(KindDiscriminant::Function)
        })
        .map(|(intro, _)| intro)
        .expect("the real `pub fn caller` declaration must be present");
    let foreign_caller = produced
        .table
        .iter()
        .find(|(_, entry)| {
            entry.sym().name == "foreign_caller"
                && entry.kind().discriminant() == Some(KindDiscriminant::Function)
        })
        .map(|(intro, _)| intro)
        .expect("the real `pub fn foreign_caller` declaration must be present");
    let many_calls = produced
        .table
        .iter()
        .find(|(_, entry)| {
            entry.sym().name == "many_calls"
                && entry.kind().discriminant() == Some(KindDiscriminant::Function)
        })
        .map(|(intro, _)| intro)
        .expect("the generated real `pub fn many_calls` declaration must be present");

    assert!(
        produced.report.rejected_facts.is_empty(),
        "the canonical fact sink rejected input: {:?}",
        produced.report.rejected_facts
    );
    assert!(
        produced.source_issues.is_empty(),
        "every recorded source span in the real fixture must materialize exactly: {:?}",
        produced.source_issues
    );

    // Source is sliced at production time from the frontend's actual file/span
    // pair.  Recompute the slice directly from the checked-in fixture bytes so
    // a rendered signature, a doc comment, or an arbitrary prefix cannot pass.
    let caller_entry = produced
        .table
        .get(caller)
        .expect("caller intro must still address a live declaration");
    assert_eq!(
        caller_entry.sym().source,
        std::path::PathBuf::from("src/lib.rs")
    );
    let source_file = fs::read_to_string(fixture.path().join("src/lib.rs"))
        .expect("fixture source must still exist");
    let expected_caller = source_file
        .get(caller_entry.sym().span.clone())
        .expect("caller span must be in the real source file");
    let excerpt = produced
        .source
        .iter()
        .find_map(|(intro, source)| (*intro == caller).then_some(source.as_str()))
        .expect("Produced must carry caller's exact source excerpt");
    assert_eq!(
        excerpt, expected_caller,
        "excerpt must equal the recorded byte span exactly"
    );
    assert!(
        excerpt.contains("target(value)"),
        "excerpt must include the full body"
    );
    assert!(
        excerpt.trim_end().ends_with('}'),
        "excerpt must include the closing body brace"
    );

    // This is the central regression assertion: the standard Produced result,
    // not `produce_with_occurrences`, must carry the resolved target.  A name
    // match would be a false-positive test; require the sealed IntroId, call
    // kind, and oracle confidence that make `find_usages` sound.
    assert!(
        produced.occurrences.iter().any(|(owner, occurrence)| {
            *owner == caller
                && occurrence.target.package == lineage
                && occurrence.target.intro == target
                && occurrence.kind == ReferenceKind::FunctionCall
                && occurrence.confidence == Confidence::Oracle
        }),
        "caller → target must survive the universal Produced/seal path as an \
         Oracle-confidence FunctionCall; got {:?}",
        produced
            .occurrences
            .iter()
            .map(|(owner, occurrence)| (owner.to_hex(), occurrence.kind, occurrence.confidence))
            .collect::<Vec<_>>()
    );

    assert!(
        produced.occurrences.iter().any(|(owner, occurrence)| {
            *owner == foreign_caller
                && occurrence.target == foreign_target
                && occurrence.kind == ReferenceKind::FunctionCall
                && occurrence.confidence == Confidence::Oracle
        }),
        "a resolved cross-package call must use the same occurrence stream as a local call"
    );
    assert_eq!(
        produced
            .occurrences
            .iter()
            .filter(|(owner, occurrence)| {
                *owner == many_calls
                    && occurrence.target.package == lineage
                    && occurrence.target.intro == target
                    && occurrence.kind == ReferenceKind::FunctionCall
            })
            .count(),
        600,
        "the universal reference walk must not truncate large real function bodies"
    );

    // Occurrences and body facts are deliberately not two producer-specific
    // exports. Sealing projects this same resolved call into the caller's
    // oracle body, retaining its durable target and relative span.
    let facts = produced
        .bodies
        .iter()
        .find_map(|(owner, body)| (*owner == caller).then(|| body.facts()))
        .flatten()
        .expect("a caller with a resolved occurrence must receive semantic body facts");
    assert_eq!(facts.language, Language::Rust);
    assert!(
        facts.merge.oracle_ran,
        "resolved calls require the oracle tier"
    );
    assert!(
        !facts.merge.treesitter_ran,
        "the Rust producer must report the actual oracle-only tier until it emits syntax facts"
    );
    assert!(
        facts.oracle.calls.iter().any(|call| {
            call.target.as_ref().is_some_and(|target_ref| {
                target_ref.package == lineage && target_ref.intro == target
            }) && call.kind == ReferenceKind::FunctionCall
                && call.confidence == Confidence::Oracle
        }),
        "body facts must carry the same resolved caller → target relation: {:?}",
        facts.oracle.calls
    );
}
