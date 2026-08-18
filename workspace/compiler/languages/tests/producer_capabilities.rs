//! What each producer actually records, declared once and checked against the
//! source.
//!
//! # Why this test exists
//!
//! Field report, 2026-08-16, against a real Go service indexed by `lindsey`:
//!
//! > `refs` (both in and out) returned `~refs:not_recorded(go)` on every call —
//! > a function (`RegisterRoutes`) and a struct (`auth.Client`) alike. "Who
//! > calls this" and "what does this reference" are simply unanswerable for Go
//! > in this build. … `subtypes` also came back empty for `AuthDeps →
//! > auth.Client`, even though `main.go` visibly passes `authClient` where
//! > `AuthDeps` is expected.
//!
//! Both gaps were real, both were per-language, and neither was discoverable
//! without running the product against a real package in that language. The
//! sentinels this workspace added (`not_recorded`, `unavailable(NoRuntime)`)
//! made them *visible* once hit — a large improvement over silence — but
//! visible-on-use is not the same as caught-in-CI.
//!
//! This test is the caught-in-CI half. It states, per language, which
//! relational facts that producer records, and checks the claim against the
//! producer's own source. A producer that gains a capability, loses one, or a
//! new language that arrives with neither, all fail here until the table is
//! updated — so the answer to "does `refs` work for Go?" lives in one readable
//! place instead of being rediscovered by an agent mid-investigation.
//!
//! # Why it scans source instead of running producers
//!
//! The behavioural test — lower a real package per language and assert the
//! edges — is the stronger one and it exists per language where the toolchain
//! allows (`tests/go/`, `tests/typescript/`, …). But those need a Go
//! toolchain, a JDK, `dotnet`, libclang: on a host missing any of them the
//! test skips, and a skipped test is exactly how a whole language's coverage
//! went unnoticed. This one runs everywhere, needs nothing installed, and
//! cannot skip.
//!
//! It is deliberately coarse: it proves the *call exists*, not that it fires
//! on any particular input. That is the right granularity for a drift guard —
//! finer assertions belong in the per-language suites, and duplicating them
//! here would make this file fail for reasons that are not drift.

use std::path::{Path, PathBuf};

/// One relational fact a producer may or may not record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Capability {
    /// Cross-symbol occurrences: what `refs` (both directions) reads. Absent
    /// means every `refs` call for that language answers `not_recorded`.
    Occurrences,
    /// `super_types` on a record: what the graph's `subtypes` edge reads.
    /// Absent means "who implements this interface" is unanswerable.
    SuperTypes,
}

impl Capability {
    /// The call whose presence in a producer's source *is* the capability.
    fn markers(self) -> &'static [&'static str] {
        match self {
            Self::Occurrences => &[".record_occurrence(", ".record_foreign_occurrence("],
            Self::SuperTypes => &[".super_types("],
        }
    }

    /// What a reader loses when this is absent — quoted into the failure so
    /// whoever trips this test knows what they changed for a user.
    fn user_visible_loss(self) -> &'static str {
        match self {
            Self::Occurrences => "`refs` answers `not_recorded` for every symbol in this language",
            Self::SuperTypes => "the graph's `subtypes` edge is empty, so \
                                 \"who implements this interface\" is unanswerable",
        }
    }
}

/// The declared capability table.
///
/// **Update this deliberately.** A change here is a change to what an agent
/// can ask about a language, and the surfaces that advertise it
/// (`schema.graphql`'s edge notes, the `graph_schema` card, and
/// `ReferenceCoverage`) have to agree.
/// `rust` is the only producer with no `super_types`, and that is correct
/// rather than a gap: Rust states implementation through `impl` blocks, which
/// the graph serves as `Trait.implementors` (`Impl.of`). Every other language
/// names its supertypes on the record itself, which is what `subtypes` reads.
/// The two edges are the same question asked of two different data shapes —
/// see `schema.graphql`'s notes on both.
const DECLARED: &[(&str, &[Capability])] = &[
    ("rust", &[Capability::Occurrences]),
    ("go", &[Capability::Occurrences, Capability::SuperTypes]),
    ("java", &[Capability::Occurrences, Capability::SuperTypes]),
    ("csharp", &[Capability::Occurrences, Capability::SuperTypes]),
    ("python", &[Capability::Occurrences, Capability::SuperTypes]),
    ("typescript", &[Capability::Occurrences, Capability::SuperTypes]),
    ("clang", &[Capability::Occurrences, Capability::SuperTypes]),
];

fn producer_src(language: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join(language)
}

/// Every `.rs` file under a producer's source tree, with line comments
/// stripped.
///
/// Comments are stripped because this crate documents its own call sites
/// heavily — several modules *mention* `record_occurrence` in prose while not
/// calling it, and counting those would make the table pass for a producer
/// that only ever wrote about the capability. That is the precise failure this
/// test exists to catch, so it must not be able to make it.
fn code_of(root: &Path) -> String {
    fn walk(dir: &Path, out: &mut String) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs")
                && let Ok(text) = std::fs::read_to_string(&path) {
                    for line in text.lines() {
                        let code = line.split("//").next().unwrap_or("");
                        out.push_str(code);
                        out.push('\n');
                    }
                }
        }
    }
    let mut out = String::new();
    walk(root, &mut out);
    out
}

fn records(code: &str, capability: Capability) -> bool {
    capability
        .markers()
        .iter()
        .any(|marker| code.contains(marker))
}

/// Every declared language's source must exist.
///
/// Guards the table against a rename silently turning a real check into a
/// vacuous one: `code_of` on a missing directory returns an empty string, and
/// an empty string records nothing, so a typo'd language name would look like
/// a producer that legitimately lost every capability.
#[test]
fn every_declared_language_has_a_producer() {
    for (language, _) in DECLARED {
        let src = producer_src(language);
        assert!(
            src.is_dir(),
            "no producer source at {}; the capability table names a language \
             this crate does not have",
            src.display(),
        );
    }
}

/// The table must not silently omit a producer.
///
/// A new language arriving with no row is the shape of the original defect:
/// nobody noticed Go answered `not_recorded` because nothing anywhere claimed
/// it should answer anything else.
#[test]
fn every_producer_appears_in_the_capability_table() {
    // Modules under `src/` that are not language producers.
    const NOT_PRODUCERS: &[&str] = &["oracle"];

    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let entries = std::fs::read_dir(&src).expect("the crate has a src/ directory");

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if NOT_PRODUCERS.contains(&name) {
            continue;
        }
        assert!(
            DECLARED.iter().any(|(language, _)| *language == name),
            "producer `{name}` has no row in the capability table. Add one \
             stating what it records — a language that arrives undeclared is \
             how `refs` stayed unanswerable for Go without anyone noticing.",
        );
    }
}

/// A producer that declares a capability must actually call for it.
#[test]
fn declared_capabilities_are_present_in_the_producer() {
    for (language, capabilities) in DECLARED {
        let code = code_of(&producer_src(language));
        for capability in *capabilities {
            assert!(
                records(&code, *capability),
                "`{language}` declares {capability:?} but its source contains \
                 none of {:?}. If the capability was removed, update the \
                 table — and note what the user loses: {}",
                capability.markers(),
                capability.user_visible_loss(),
            );
        }
    }
}

/// ...and a producer that does NOT declare one must not have it either.
///
/// The direction that matters least for users and most for honesty: a
/// capability that quietly appeared while the table (and the docs derived from
/// it) still say it is missing means the product under-promises, and an agent
/// told `not_recorded` stops asking a question the index could actually
/// answer.
#[test]
fn undeclared_capabilities_are_absent_from_the_producer() {
    const ALL: &[Capability] = &[Capability::Occurrences, Capability::SuperTypes];

    for (language, capabilities) in DECLARED {
        let code = code_of(&producer_src(language));
        for capability in ALL {
            if capabilities.contains(capability) {
                continue;
            }
            assert!(
                !records(&code, *capability),
                "`{language}` records {capability:?} but the table does not \
                 declare it. Add it — the surfaces that tell an agent what is \
                 answerable read from this table, and an undeclared capability \
                 is one nobody will ask for.",
            );
        }
    }
}
