//! Adversarial contract tests for `nudox-store`.
//!
//! These are deliberately written *against* the crate rather than alongside it.
//! Where the in-module unit tests check that each piece does what its author
//! intended, these tests re-derive the expected answers from `nudox-ir`
//! directly and compare — so a shared misunderstanding between the index
//! builder and its own unit test cannot hide here.
//!
//! They also pin the properties the layers above depend on but cannot check
//! for themselves: the `IrSource` stream's ordering guarantees (GUI-LOCAL-PLAN
//! §L3.2), and the `Send + Sync + 'static` shape that lets `nudox-graph` build
//! `'static` vertices and the engine move the corpus across runtimes (§L4.1).

use std::collections::{HashMap, HashSet};

use futures::StreamExt as _;
use nudox_ir::change::{IntroId, PackageLineageId, StableRef};
use nudox_ir::kind::Kind;
use nudox_ir::view::IrView;
use nudox_ir::vocab::Confidence;
use nudox_store::prelude::*;
use nudox_store::source::fixtures::{FixtureSource, build_rich_view, rich_lineage};

// ── Type-level guarantees ────────────────────────────────────────────────────

/// `Corpus` must be shareable across the engine's runtimes without ceremony.
///
/// This is a compile-time assertion: if `Corpus` ever gains a non-`Send` field
/// the whole engine design (LR-9) stops working, and it should fail *here*
/// with a clear message rather than at some distant call site.
#[test]
fn corpus_is_send_sync_static() {
    fn assert_shape<T: Send + Sync + 'static>() {}
    assert_shape::<Corpus>();
    assert_shape::<PackageView>();
    assert_shape::<EntryRef>();
}

// ── Stream protocol (§L3.2) ──────────────────────────────────────────────────

/// Every package must announce itself before it is delivered, and a package
/// that fails must never subsequently be reported ready.
///
/// The GUI relies on `Discovered` to paint a placeholder row; if `Ready` could
/// arrive first the row would appear already-filled and then re-animate.
#[tokio::test]
async fn load_stream_orders_discovered_before_ready_per_package() {
    let source = FixtureSource::both();
    let mut stream = source.load(LoadRequest::default());

    let mut discovered: Vec<PackageLineageId> = Vec::new();
    let mut ready: Vec<PackageLineageId> = Vec::new();
    let mut failed: HashSet<PackageLineageId> = HashSet::new();

    while let Some(event) = stream.next().await {
        match event.expect("fixture source is infallible at the stream level") {
            LoadEvent::Discovered { lineage, .. } => discovered.push(lineage),
            LoadEvent::Progress { lineage, .. } => {
                assert!(
                    discovered.contains(&lineage),
                    "Progress for {lineage} arrived before its Discovered"
                );
            }
            LoadEvent::Ready { package } => {
                let lineage = package.lineage().clone();
                assert!(
                    discovered.contains(&lineage),
                    "Ready for {lineage} arrived before its Discovered"
                );
                assert!(
                    !failed.contains(&lineage),
                    "{lineage} was reported Ready after it had already Failed"
                );
                ready.push(lineage);
            }
            LoadEvent::Failed { lineage, .. } => {
                failed.insert(lineage);
            }
            _ => {}
        }
    }

    assert!(!ready.is_empty(), "fixture source produced no packages");
    assert_eq!(
        discovered.len(),
        ready.len() + failed.len(),
        "every discovered package must end in exactly one terminal event"
    );
}

// ── Index correctness, re-derived independently ──────────────────────────────

/// Recompute the usages postings straight from the occurrence facts and compare.
///
/// `PackageIndexes` filters occurrences to `Confidence >= Index` (the
/// "graph-worthy floor" ported from `registry::graph::reverse_index`). This
/// test rebuilds that mapping from `IrView::all_occurrences` with no knowledge
/// of how the index does it, so an off-by-one in the confidence comparison —
/// `>` instead of `>=`, say — cannot pass both implementations.
#[test]
fn usages_postings_match_independently_computed_truth() {
    let view = build_rich_view();
    let package = rich_lineage();
    let pv = PackageView::build(view, Provenance::TrustedLocal);

    let mut expected: HashMap<StableRef, Vec<IntroId>> = HashMap::new();
    for (owner, occ) in pv.view().all_occurrences() {
        if occ.confidence >= Confidence::Index {
            expected.entry(occ.target.clone()).or_default().push(owner);
        }
    }
    for owners in expected.values_mut() {
        owners.sort();
        owners.dedup();
    }

    assert!(
        !expected.is_empty(),
        "fixture must contain graph-worthy occurrences or this test proves nothing"
    );

    for (target, owners) in &expected {
        assert_eq!(
            pv.indexes().usages_of(target),
            owners.as_slice(),
            "usages_of({target}) disagreed with the independently computed postings"
        );
    }

    // And the converse: nothing below the floor may have leaked in.
    for (_owner, occ) in pv.view().all_occurrences() {
        if occ.confidence < Confidence::Index && !expected.contains_key(&occ.target) {
            assert!(
                pv.indexes().usages_of(&occ.target).is_empty(),
                "occurrence below the Index confidence floor leaked into usages for {}",
                occ.target
            );
        }
    }

    let _ = package;
}

/// Every entry in the view must be reachable through the corpus by `StableRef`.
///
/// `StableRef` is the one key (LR-1) — it is the MCP argument, the tab key and
/// the vertex id. If any entry were unreachable by it, those surfaces would
/// have symbols they can display but cannot link to.
#[tokio::test]
async fn every_entry_is_reachable_by_stable_ref() {
    let view = build_rich_view();
    let lineage = view.package().clone();
    let expected: Vec<IntroId> = view.entries().map(|(intro, _)| intro).collect();

    let corpus = Corpus::new();
    corpus
        .insert(std::sync::Arc::new(PackageView::build(
            view,
            Provenance::TrustedLocal,
        )))
        .await;

    assert!(!expected.is_empty());
    for intro in expected {
        let key = StableRef::new(lineage.clone(), intro);
        let entry_ref = corpus
            .entry(&key)
            .await
            .unwrap_or_else(|| panic!("{key} was not reachable through the corpus"));
        assert!(
            entry_ref.get().is_some(),
            "{key} resolved to an EntryRef whose entry is missing"
        );
    }
}

/// Precomputed paths must exist for every entry, and be unique per entry.
///
/// Search rows and breadcrumbs render these directly (§1.1.4: no string work in
/// `render`), so a missing path means a blank row and a duplicated path means
/// two different symbols that look identical to the user.
#[test]
fn precomputed_paths_are_total_and_unique() {
    let pv = PackageView::build(build_rich_view(), Provenance::TrustedLocal);

    let mut seen: HashMap<String, IntroId> = HashMap::new();
    for (intro, _entry) in pv.view().entries() {
        let path = pv
            .indexes()
            .path_of(intro)
            .unwrap_or_else(|| panic!("no precomputed path for {intro:?}"));
        if let Some(previous) = seen.insert(path.to_string(), intro) {
            panic!("path {path:?} is shared by {previous:?} and {intro:?}");
        }
    }
}

// ── Concurrency ──────────────────────────────────────────────────────────────

/// Concurrent readers must not deadlock or starve, including while a write
/// lands between them.
///
/// The engine reads the corpus from the query `LocalSet` while the loader is
/// still inserting packages; if `Corpus`'s locking were re-entrant-unsafe this
/// is where it would hang rather than at some unlucky moment in the GUI.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_reads_during_insert_do_not_deadlock() {
    let corpus = Corpus::new();
    let view = build_rich_view();
    let lineage = view.package().clone();
    corpus
        .insert(std::sync::Arc::new(PackageView::build(
            view,
            Provenance::TrustedLocal,
        )))
        .await;

    let readers = (0..8).map(|_| {
        let corpus = corpus.clone();
        let lineage = lineage.clone();
        tokio::spawn(async move {
            for _ in 0..200 {
                assert!(corpus.package(&lineage).await.is_some());
                let _ = corpus.packages().await;
            }
        })
    });

    // A late insert racing the readers — the common shape during workspace load.
    let writer = {
        let corpus = corpus.clone();
        tokio::spawn(async move {
            let pv = PackageView::build(
                nudox_store::source::fixtures::build_perf_view(),
                Provenance::TrustedLocal,
            );
            corpus.insert(std::sync::Arc::new(pv)).await;
        })
    };

    for reader in readers {
        reader.await.expect("reader task panicked or deadlocked");
    }
    writer.await.expect("writer task panicked or deadlocked");
    assert_eq!(corpus.len().await, 2);
}

// ── Scale ────────────────────────────────────────────────────────────────────

/// The 10 000-symbol fixture must index within a budget the GUI can absorb.
///
/// This is not a micro-benchmark; it is a guard against an accidental
/// quadratic. Index construction happens off the GPUI thread, but a workspace
/// with 250 dependencies multiplies whatever this costs by 250.
#[test]
fn perf_corpus_indexes_without_quadratic_blowup() {
    let view = nudox_store::source::fixtures::build_perf_view();
    let entry_count = view.table().len();
    assert!(
        entry_count >= 10_000,
        "perf fixture shrank to {entry_count}; the guard is meaningless"
    );

    let started = std::time::Instant::now();
    let pv = PackageView::build(view, Provenance::TrustedLocal);
    let elapsed = started.elapsed();

    // Generous: a linear pass over 10k entries is milliseconds. This only
    // catches an algorithmic regression, not a constant-factor one.
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "indexing {entry_count} entries took {elapsed:?}"
    );
    assert_eq!(pv.view().table().len(), entry_count);
}

// ── Kind coverage, checked against nudox-ir's own enumeration ────────────────

/// The rich fixture must exercise every `Kind` the IR can represent.
///
/// The in-module test asserts against a hand-written list of 13 variants, which
/// silently stops being exhaustive the day a 14th is added. This one walks the
/// entries and asserts the count matches `KindDiscriminant`'s own round-trip
/// range, so adding a kind to `nudox-ir` fails here until the fixture covers it.
#[test]
fn rich_fixture_covers_every_kind_the_ir_defines() {
    let view: IrView = build_rich_view();

    let mut seen = HashSet::new();
    for (_intro, entry) in view.entries() {
        if let Some(kind) = entry.kind().as_owned_kind() {
            seen.insert(discriminant_index(kind));
        }
    }

    // `KindDiscriminant` is a frozen repr(u16) enumeration starting at 1.
    let defined: HashSet<u16> = (1..=u16::MAX)
        .take_while(|n| nudox_ir::kind::KindDiscriminant::from_u16(*n).is_some())
        .collect();

    assert!(!defined.is_empty(), "could not enumerate KindDiscriminant");
    let missing: Vec<u16> = defined.difference(&seen).copied().collect();
    assert!(
        missing.is_empty(),
        "rich fixture is missing kinds {missing:?} \
         (nudox-ir defines {} kinds, fixture covers {})",
        defined.len(),
        seen.len()
    );
}

fn discriminant_index(kind: &Kind) -> u16 {
    kind.discriminant().as_u16()
}

// ── Corpus entry counts: the one authoritative home ──────────────────────────
//
// `corpus/entry-baseline.toml` is the ONLY place in this repo that states how
// many IR entries a corpus package lowers to. Everything else that quotes a
// number — LIMITATIONS.md's banners, CORPUS-SWEEP.md, CORPUS-REPORT.md,
// DOCSRS-COMPARISON.md — is prose reporting a measurement, and prose does not
// get to disagree with the measurement. Before this file existed there was no
// per-package baseline format at all, and the numbers in those documents had
// drifted three ways for the same three package versions with nothing in the
// build able to notice.
//
// The two properties that make it authoritative, both enforced below:
//
//   1. It is TOTAL over `corpus/manifest.toml`. A package listed in the
//      manifest with no row here fails `every_corpus_package_has_a_recorded_
//      entry_baseline` — it cannot be silently absent. This is the same shape
//      as `rich_fixture_covers_every_kind_the_ir_defines` above, which derives
//      its expectation from `KindDiscriminant`'s own range rather than a
//      hand-written list, so that adding a kind upstream fails here until the
//      fixture covers it. Adding a package upstream fails here until it is
//      measured.
//
//   2. Every row states an OUTCOME, and "no number" is not one of them. A row
//      is `entries = N`, or `producer_error = "…"`, or `unmeasured = "…"` —
//      three disjoint shapes, `deny_unknown_fields` on each, so a row that
//      carries a version and nothing else does not parse. Recording that we
//      have not measured something is allowed; recording nothing is not.
//
// See `corpus/entry-baseline.toml`'s own header for the file format, and
// `corpus_entry_counts_match_the_recorded_baseline` for the equality-versus-
// floor argument.

mod baseline {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use nudox_store::source::producer::PackageDescriptor;

    /// The seven package registries `corpus/manifest.toml` draws from.
    ///
    /// A closed enum rather than a `String` so that an eighth ecosystem
    /// appearing in the manifest is a *parse* failure here, naming the unknown
    /// token, rather than a row that quietly maps to no producer and is
    /// reported as an unmeasurable package. `descriptor_for` matches on it
    /// exhaustively with no wildcard, so a new variant also breaks compilation
    /// until someone says which producer serves it.
    #[derive(
        Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Deserialize,
    )]
    pub enum Ecosystem {
        #[serde(rename = "crates.io")]
        CratesIo,
        #[serde(rename = "go")]
        Go,
        #[serde(rename = "maven")]
        Maven,
        #[serde(rename = "nuget")]
        Nuget,
        #[serde(rename = "npm")]
        Npm,
        #[serde(rename = "pypi")]
        Pypi,
        #[serde(rename = "cpp")]
        Cpp,
    }

    impl Ecosystem {
        /// The token this ecosystem is spelled with in both corpus files.
        pub fn token(self) -> &'static str {
            match self {
                Self::CratesIo => "crates.io",
                Self::Go => "go",
                Self::Maven => "maven",
                Self::Nuget => "nuget",
                Self::Npm => "npm",
                Self::Pypi => "pypi",
                Self::Cpp => "cpp",
            }
        }

        /// Build the descriptor whose named constructor pairs this ecosystem
        /// with the `Language` whose producer serves it.
        ///
        /// Deliberately delegates to `PackageDescriptor`'s per-ecosystem
        /// constructors rather than reproducing the ecosystem/language pairing
        /// — that pairing is exactly what those constructors exist to make
        /// unmistakable (see their doc comments), and a second copy of it here
        /// could drift and route a package at a producer that cannot read it.
        pub fn descriptor_for(self, root: &std::path::Path, name: &str, version: &str) -> PackageDescriptor {
            match self {
                Self::CratesIo => PackageDescriptor::cargo(root, name, version),
                Self::Go => PackageDescriptor::go(root, name, version),
                Self::Maven => PackageDescriptor::maven(root, name, version),
                Self::Nuget => PackageDescriptor::nuget(root, name, version),
                Self::Npm => PackageDescriptor::npm(root, name, version),
                Self::Pypi => PackageDescriptor::pypi(root, name, version),
                Self::Cpp => PackageDescriptor::cpp(root, name, version),
            }
        }
    }

    /// The key that joins `corpus/manifest.toml` to `corpus/entry-baseline.toml`.
    ///
    /// A struct rather than a bare `(String, String, String)` so the three
    /// fields cannot be swapped at a call site — `(name, version)` and
    /// `(version, name)` are both `(String, String)` and only one is right.
    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
    pub struct CorpusKey {
        pub ecosystem: Ecosystem,
        pub name: String,
        pub version: String,
    }

    impl std::fmt::Display for CorpusKey {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}/{} {}", self.ecosystem.token(), self.name, self.version)
        }
    }

    // ── Repo layout ─────────────────────────────────────────────────────────

    /// The repository root, from this package (`crates/nudox-store`).
    pub fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    pub fn manifest_path() -> PathBuf {
        repo_root().join("corpus/manifest.toml")
    }

    pub fn baseline_path() -> PathBuf {
        repo_root().join("corpus/entry-baseline.toml")
    }

    pub fn real_crates_root() -> PathBuf {
        repo_root().join(".real-crates")
    }

    /// Filesystem-safe fixture directory name, mirroring `corpus/fetch.nu`'s
    /// `safe-dir-name` byte for byte (`/` and `:` both become `__`), so this
    /// resolves exactly the directories the fetch step wrote.
    ///
    /// Kept identical to `workspace/index/tests/common/mod.rs::safe_dir_name`.
    /// Two copies is one too many, but the alternative today is worse: that
    /// helper lives in another package's `tests/common`, which is not
    /// importable from here, and promoting it to a shared crate would mean
    /// editing `workspace/index`, outside this change's scope.
    pub fn safe_dir_name(name: &str, version: &str) -> String {
        format!("{}-{version}", name.replace('/', "__").replace(':', "__"))
    }

    // ── corpus/manifest.toml ────────────────────────────────────────────────

    #[derive(Debug, serde::Deserialize)]
    struct ManifestFile {
        packages: Vec<ManifestPackage>,
    }

    #[derive(Debug, serde::Deserialize)]
    struct ManifestPackage {
        ecosystem: Ecosystem,
        name: String,
        versions: Vec<ManifestVersion>,
    }

    #[derive(Debug, serde::Deserialize)]
    struct ManifestVersion {
        version: String,
        /// nix-base32 sha256 of the exact bytes fetched. Unused here, but its
        /// presence is why per-package equality is the right comparison — see
        /// `corpus_entry_counts_match_the_recorded_baseline`.
        #[allow(dead_code)]
        hash: String,
    }

    /// Every `(ecosystem, name, version)` the corpus manifest declares, sorted.
    ///
    /// Panics on a missing or malformed manifest: that is a repo setup defect,
    /// not a soft-skippable test outcome. A test that shrugs at an unreadable
    /// manifest and passes is the exact failure this whole section exists to
    /// remove.
    pub fn manifest_keys() -> Vec<CorpusKey> {
        let path = manifest_path();
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let parsed: ManifestFile = toml::from_str(&text)
            .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));

        let mut keys: Vec<CorpusKey> = parsed
            .packages
            .into_iter()
            .flat_map(|package| {
                let ecosystem = package.ecosystem;
                let name = package.name;
                package.versions.into_iter().map(move |version| CorpusKey {
                    ecosystem,
                    name: name.clone(),
                    version: version.version,
                })
            })
            .collect();
        keys.sort();
        keys
    }

    // ── corpus/entry-baseline.toml ──────────────────────────────────────────

    /// A package version that lowered, and the exact entry count it produced.
    #[derive(Debug, Clone, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct LoweredRow {
        pub version: String,
        pub entries: usize,
    }

    /// A package version whose producer was run and returned an error. The
    /// failure is recorded rather than omitted, so "we tried and it broke" and
    /// "nobody ever tried" stay distinguishable — the same distinction
    /// `ProducerRegistry::with_all_available` refuses to blur for Python.
    #[derive(Debug, Clone, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct FailedRow {
        pub version: String,
        pub producer_error: String,
    }

    /// A package version nobody has measured yet, and why not.
    ///
    /// This is measurement *debt*, and it is counted and pinned by
    /// `corpus_baseline_measurement_debt_does_not_grow` so it cannot quietly
    /// expand: doctrine §8's "counted, bounded, visible" rule applied to the
    /// absence of a measurement rather than to a repair.
    #[derive(Debug, Clone, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct UnmeasuredRow {
        pub version: String,
        pub unmeasured: String,
    }

    /// The three — and only three — things a baseline row may say.
    ///
    /// `untagged` + `deny_unknown_fields` on each variant makes the shapes
    /// disjoint: a row carrying both `entries` and `unmeasured` matches
    /// neither, and a row carrying only `version` matches neither. There is no
    /// representable row that names a package version without also saying what
    /// we know about it.
    #[derive(Debug, Clone, serde::Deserialize)]
    #[serde(untagged)]
    pub enum VersionBaseline {
        Lowered(LoweredRow),
        Failed(FailedRow),
        Unmeasured(UnmeasuredRow),
    }

    impl VersionBaseline {
        pub fn version(&self) -> &str {
            match self {
                Self::Lowered(row) => &row.version,
                Self::Failed(row) => &row.version,
                Self::Unmeasured(row) => &row.version,
            }
        }

        /// One-line rendering used in failure messages and in the regenerated
        /// file, so the two can never describe the same row differently.
        pub fn render(&self) -> String {
            match self {
                Self::Lowered(row) => {
                    format!("{{ version = \"{}\", entries = {} }}", row.version, row.entries)
                }
                Self::Failed(row) => format!(
                    "{{ version = \"{}\", producer_error = \"{}\" }}",
                    row.version,
                    escape(&row.producer_error)
                ),
                Self::Unmeasured(row) => format!(
                    "{{ version = \"{}\", unmeasured = \"{}\" }}",
                    row.version,
                    escape(&row.unmeasured)
                ),
            }
        }
    }

    /// TOML basic-string escaping for the two free-text fields. Producer error
    /// text is arbitrary — it has contained quotes and backslashes — and an
    /// unescaped one would emit a baseline file that does not parse.
    pub fn escape(text: &str) -> String {
        text.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', " ")
            .replace('\r', " ")
    }

    #[derive(Debug, serde::Deserialize)]
    struct BaselineFile {
        packages: Vec<BaselinePackage>,
    }

    #[derive(Debug, serde::Deserialize)]
    struct BaselinePackage {
        ecosystem: Ecosystem,
        name: String,
        versions: Vec<VersionBaseline>,
    }

    /// The recorded baseline, keyed exactly as the manifest is keyed.
    ///
    /// Panics if the file is missing or malformed. A baseline that cannot be
    /// read must not degrade into "no expectations" — that is how the numbers
    /// got into their present state.
    pub fn load_baseline() -> BTreeMap<CorpusKey, VersionBaseline> {
        let path = baseline_path();
        let text = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "read {}: {error}\n\
                 This file is the authoritative record of corpus entry counts. \
                 Regenerate it with:\n  \
                 UPDATE_CORPUS_BASELINE=1 cargo test -p nudox-store --features fixtures \
                 --test corpus_contract corpus_entry_counts -- --ignored --nocapture",
                path.display()
            )
        });
        let parsed: BaselineFile = toml::from_str(&text).unwrap_or_else(|error| {
            panic!(
                "parse {}: {error}\n\
                 Every `versions` entry must be exactly one of:\n  \
                 {{ version = \"X\", entries = N }}\n  \
                 {{ version = \"X\", producer_error = \"…\" }}\n  \
                 {{ version = \"X\", unmeasured = \"…\" }}",
                path.display()
            )
        });

        let mut out: BTreeMap<CorpusKey, VersionBaseline> = BTreeMap::new();
        for package in parsed.packages {
            for row in package.versions {
                let key = CorpusKey {
                    ecosystem: package.ecosystem,
                    name: package.name.clone(),
                    version: row.version().to_owned(),
                };
                if let Some(previous) = out.insert(key.clone(), row) {
                    panic!(
                        "{} appears twice in {} (first: {})",
                        key,
                        baseline_path().display(),
                        previous.render()
                    );
                }
            }
        }
        out
    }

    /// Render a whole baseline file in `corpus/manifest.toml`'s own style.
    ///
    /// Hand-rendered rather than `toml::to_string`: the manifest writes its
    /// versions as inline tables and a serializer would expand them into
    /// `[[packages.versions]]` sub-tables, quadrupling the file's length and
    /// making the one thing a reviewer needs to see — which counts moved —
    /// harder to see, not easier.
    pub fn render_baseline(rows: &BTreeMap<CorpusKey, VersionBaseline>) -> String {
        let mut out = String::from(BASELINE_HEADER);
        let mut current: Option<(Ecosystem, &str)> = None;
        for (key, row) in rows {
            if current != Some((key.ecosystem, key.name.as_str())) {
                if current.is_some() {
                    out.push_str("]\n");
                }
                out.push_str(&format!(
                    "\n[[packages]]\necosystem = \"{}\"\nname = \"{}\"\nversions = [\n",
                    key.ecosystem.token(),
                    key.name
                ));
                current = Some((key.ecosystem, key.name.as_str()));
            }
            out.push_str(&format!("  {},\n", row.render()));
        }
        if current.is_some() {
            out.push_str("]\n");
        }
        out
    }

    pub const BASELINE_HEADER: &str = "\
# Corpus entry baseline — the ONE authoritative record of how many IR entries
# each package in `corpus/manifest.toml` lowers to.
#
# DO NOT hand-edit a number here. Every figure is produced by running the real
# producer over the real, hash-pinned fixture; a number typed in by hand is
# indistinguishable from a measured one and destroys the only property that
# makes this file worth having.
#
# Regenerate with (this rewrites the file AND fails the run, deliberately —
# review `git diff corpus/entry-baseline.toml` before re-running clean):
#
#   UPDATE_CORPUS_BASELINE=1 cargo test -p nudox-store --features fixtures \\
#     --test corpus_contract corpus_entry_counts -- --ignored --nocapture
#
# Keyed by (ecosystem, name, version), exactly as `corpus/manifest.toml` is.
# Every manifest version must appear here — `nudox-store`'s
# `every_corpus_package_has_a_recorded_entry_baseline` fails otherwise.
#
# Each `versions` entry is exactly one of three shapes:
#
#   { version = \"X\", entries = N }            producer ran, sealed N entries
#   { version = \"X\", producer_error = \"…\" }   producer ran and failed
#   { version = \"X\", unmeasured = \"…\" }       never measured, and why not
#
# `unmeasured` is measurement debt. Its total is pinned by
# `corpus_baseline_measurement_debt_does_not_grow`, so the debt cannot grow
# without someone raising the pin on purpose.
";
}

/// How many corpus package versions have never been measured.
///
/// This is a debt pin, not a target: it exists so that adding a package to
/// `corpus/manifest.toml` and parking it as `unmeasured` is a *visible* act
/// that reddens the suite until someone edits this number on purpose. It must
/// only ever be lowered. Raising it is a decision, and it should be argued for
/// in the commit that raises it.
///
/// 131 of the corpus's 154 package versions, as of 2026-08-07. The 23 that are
/// measured are all of `crates.io`; the other six ecosystems' producers have
/// not been swept through this baseline yet. Their per-ecosystem sweeps
/// (`workspace/compiler/languages/*/tests/corpus_sweep.rs`) each hardcode their
/// own copy of the package list, which is the duplication this file is meant to
/// end — folding them in is what should drive this number down.
const UNMEASURED_CORPUS_VERSIONS: usize = 131;

/// Every package version in `corpus/manifest.toml` must have a row in
/// `corpus/entry-baseline.toml`, and vice versa.
///
/// This is the property that makes the baseline authoritative rather than
/// merely present. It derives its expectation from the manifest — the same file
/// `corpus/fetch.nu` fetches from — instead of from a hand-written list, so a
/// package added to the corpus fails here until it has been measured, exactly
/// as `rich_fixture_covers_every_kind_the_ir_defines` fails when `nudox-ir`
/// gains a kind the fixture does not cover.
///
/// Cheap on purpose: it reads two files and compares two sorted key sets, so it
/// runs in the default suite and cannot be skipped into irrelevance.
#[test]
fn every_corpus_package_has_a_recorded_entry_baseline() {
    let ((manifest, recorded), _cost) = nudox_test_support::measured(
        "corpus/baseline-coverage",
        &baseline::repo_root().join("corpus"),
        || (baseline::manifest_keys(), baseline::load_baseline()),
    );

    assert!(
        !manifest.is_empty(),
        "corpus/manifest.toml declared no packages — a coverage check over an \
         empty set proves nothing"
    );

    let missing: Vec<String> = manifest
        .iter()
        .filter(|key| !recorded.contains_key(key))
        .map(ToString::to_string)
        .collect();
    assert!(
        missing.is_empty(),
        "{} corpus package version(s) are in corpus/manifest.toml with no row in \
         corpus/entry-baseline.toml:\n{}\n\n\
         Every corpus package must state an outcome — a measured count, a recorded \
         producer failure, or an explicit `unmeasured = \"why\"`. Measure them with:\n  \
         UPDATE_CORPUS_BASELINE=1 cargo test -p nudox-store --features fixtures \
         --test corpus_contract corpus_entry_counts -- --ignored --nocapture",
        missing.len(),
        missing.join("\n"),
    );

    let stale: Vec<String> = recorded
        .keys()
        .filter(|key| !manifest.contains(key))
        .map(ToString::to_string)
        .collect();
    assert!(
        stale.is_empty(),
        "{} row(s) in corpus/entry-baseline.toml name package versions that \
         corpus/manifest.toml no longer lists:\n{}\n\n\
         A baseline for a package that is not in the corpus is a number nothing \
         re-derives — delete the rows or restore the manifest entries.",
        stale.len(),
        stale.join("\n"),
    );
}

/// The number of corpus packages nobody has measured must not grow silently.
///
/// Coverage alone is satisfiable by writing `unmeasured` on everything, which
/// would make the baseline total and worthless. Pinning the count closes that:
/// parking a new package as debt fails here until the pin is raised on purpose,
/// which puts the decision in a diff where it can be argued with. This is
/// doctrine §8's counted/bounded/visible rule applied to a missing measurement
/// — the same defect class as a silent repair, since both present a degraded
/// state as the good one.
#[test]
fn corpus_baseline_measurement_debt_does_not_grow() {
    let recorded = baseline::load_baseline();
    let unmeasured: Vec<String> = recorded
        .iter()
        .filter(|(_, row)| matches!(row, baseline::VersionBaseline::Unmeasured(_)))
        .map(|(key, _)| key.to_string())
        .collect();

    assert!(
        unmeasured.len() <= UNMEASURED_CORPUS_VERSIONS,
        "corpus measurement debt grew from {} to {} unmeasured package version(s).\n\
         Measure the new ones, or raise UNMEASURED_CORPUS_VERSIONS deliberately and \
         say why in the commit. Currently unmeasured:\n{}",
        UNMEASURED_CORPUS_VERSIONS,
        unmeasured.len(),
        unmeasured.join("\n"),
    );

    assert_eq!(
        unmeasured.len(),
        UNMEASURED_CORPUS_VERSIONS,
        "corpus measurement debt fell to {} — lower UNMEASURED_CORPUS_VERSIONS to \
         match so the improvement is locked in and cannot silently regress",
        unmeasured.len(),
    );
}

/// Every corpus fixture on disk must lower to exactly the entry count
/// `corpus/entry-baseline.toml` records for it.
///
/// # Why exact equality, and not a floor or a tolerance band
///
/// This was the design decision in this change, so here is the argument.
///
/// A **floor** (`>= N`) is the tempting choice because it survives producer
/// improvements. It is the wrong one here, and this repo has the receipts: the
/// defect that motivated all of this was `memchr` reporting **11,329** entries
/// for three consecutive generations because `cargo metadata` was failing and
/// upstream substituted `--no-deps` metadata (doctrine §8). That number was
/// roughly **eight times too high**. A floor passes it. A floor passes it every
/// day, forever, and reports green. And when L38 later removed the `rustc-std-
/// workspace-*` shim entries and the true count dropped by ~8×, a floor would
/// have gone red on a *correct* change while having stayed green through the
/// whole period the number was a lie. A floor is wrong in both directions here.
///
/// A **tolerance band** is worse than it looks for the same reason: 11,329 was
/// stable to the entry across three package versions, so any band centred on it
/// also passes. Bands catch noise. There is no noise to catch — see below.
///
/// **Equality is available because the input is pinned.** Every row in
/// `corpus/manifest.toml` carries a sha256 of the exact bytes fetched, and
/// `corpus/fetch.nu` verifies it before extracting. The fixture is therefore
/// byte-identical on every machine, and the producer is deterministic over it.
/// A changed count is never noise; it is always a change in *our* code, and it
/// is always worth a human's attention. Doctrine §8 puts it directly: a
/// measurement that does not move when its input should move it is a bug
/// report, and suspicious agreement deserves the same scrutiny as suspicious
/// disagreement. Only equality surfaces both.
///
/// **The brittleness objection is answered by legibility, not by loosening.**
/// The real cost of equality is that one producer improvement can redden a
/// hundred rows at once, and the human response to a hundred red rows is to
/// regenerate without reading — which is precisely how 11,329 survived three
/// blessings. So the failure below reports every drifted row on one line each,
/// sorted by the size of the change with percentages, so a uniform 8× collapse
/// and a single +1 look nothing alike at a glance. And regeneration rewrites
/// the file *and fails the run*, following `workspace/ir/model/tests/golden.rs`,
/// so new numbers cannot land without passing through a `git diff` someone has
/// to look at.
///
/// The baseline also pins the *outcome kind*, not just the number, so a package
/// that silently flips from lowering to failing is a failure here even though
/// no count changed.
#[test]
#[ignore = "runs every language producer over the whole ~154-package .real-crates corpus; \
            tens of minutes and needs the fixtures fetched (corpus/fetch.nu) plus each \
            producer's toolchain"]
fn corpus_entry_counts_match_the_recorded_baseline() {
    let manifest = baseline::manifest_keys();
    let fixtures = baseline::real_crates_root();

    assert!(
        fixtures.is_dir(),
        "no corpus at {}. This test measures real packages; with no fixtures it \
         would assert nothing and report PASS, which is the failure mode it exists \
         to prevent. Fetch them with `nu corpus/fetch.nu` (see corpus/README.md).",
        fixtures.display(),
    );

    let present: Vec<(baseline::CorpusKey, std::path::PathBuf)> = manifest
        .iter()
        .map(|key| {
            (
                key.clone(),
                fixtures.join(baseline::safe_dir_name(&key.name, &key.version)),
            )
        })
        .collect();

    let absent: Vec<String> = present
        .iter()
        .filter(|(_, dir)| !dir.is_dir())
        .map(|(key, dir)| format!("{key} (expected at {})", dir.display()))
        .collect();
    assert!(
        absent.is_empty(),
        "{} of {} manifest package version(s) have no fixture on disk:\n{}\n\n\
         Measuring only the subset that happens to be present would let a shrinking \
         corpus look like a passing suite. Fetch the rest with `nu corpus/fetch.nu`.",
        absent.len(),
        manifest.len(),
        absent.join("\n"),
    );

    let regenerating = std::env::var("UPDATE_CORPUS_BASELINE").is_ok();
    let scope = measurement_scope();
    if let Some(only) = &scope {
        assert!(
            regenerating,
            "NUDOX_BASELINE_ECOSYSTEMS={} was set without UPDATE_CORPUS_BASELINE=1. \
             Scoping is a regeneration convenience only: narrowing what a verification \
             run checks is indistinguishable from making it pass, so verification is \
             always total.",
            only.iter()
                .map(|e| e.token())
                .collect::<Vec<_>>()
                .join(",")
        );
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build measurement runtime");

    let mut measured_rows: std::collections::BTreeMap<
        baseline::CorpusKey,
        baseline::VersionBaseline,
    > = std::collections::BTreeMap::new();

    for (key, dir) in &present {
        if regenerating && scope.as_ref().is_some_and(|only| !only.contains(&key.ecosystem)) {
            continue;
        }
        let (row, _cost) = nudox_test_support::measured(
            &format!("corpus/{}/{}-{}", key.ecosystem.token(), key.name, key.version),
            dir,
            || measure_one(&runtime, key, dir),
        );
        eprintln!("baseline {key} => {}", row.render());
        measured_rows.insert(key.clone(), row);
    }

    if regenerating {
        // Rows outside the requested scope are carried forward from the
        // existing file rather than discarded — re-measuring 154 packages to
        // correct one number is not a thing anyone will actually do, and a
        // regeneration that silently dropped the other ecosystems' numbers
        // would be a worse outcome than the drift it was fixing.
        let carried = std::panic::catch_unwind(baseline::load_baseline).unwrap_or_default();
        for (key, row) in carried {
            measured_rows.entry(key).or_insert(row);
        }
        for key in &manifest {
            measured_rows
                .entry(key.clone())
                .or_insert(baseline::VersionBaseline::Unmeasured(baseline::UnmeasuredRow {
                    version: key.version.clone(),
                    unmeasured: format!(
                        "not yet measured; re-run with NUDOX_BASELINE_ECOSYSTEMS={}",
                        key.ecosystem.token()
                    ),
                }));
        }

        let rendered = baseline::render_baseline(&measured_rows);
        let path = baseline::baseline_path();
        std::fs::write(&path, &rendered)
            .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
        panic!(
            "UPDATE_CORPUS_BASELINE is set — {} rewritten with {} row(s).\n\
             This run fails on purpose, matching workspace/ir/model/tests/golden.rs: \
             review `git diff corpus/entry-baseline.toml` and re-run without \
             UPDATE_CORPUS_BASELINE to confirm it is green.",
            path.display(),
            measured_rows.len(),
        );
    }

    let recorded = baseline::load_baseline();
    let mut drifted: Vec<(i128, String)> = Vec::new();
    for (key, actual) in &measured_rows {
        let Some(expected) = recorded.get(key) else {
            drifted.push((i128::MAX, format!("{key}: no baseline row (measured {})", actual.render())));
            continue;
        };
        match (expected, actual) {
            (
                baseline::VersionBaseline::Lowered(want),
                baseline::VersionBaseline::Lowered(got),
            ) if want.entries == got.entries => {}
            (
                baseline::VersionBaseline::Lowered(want),
                baseline::VersionBaseline::Lowered(got),
            ) => {
                let delta = got.entries as i128 - want.entries as i128;
                let percent = if want.entries == 0 {
                    f64::INFINITY
                } else {
                    delta as f64 * 100.0 / want.entries as f64
                };
                drifted.push((
                    delta.abs(),
                    format!(
                        "{key}: {} -> {} ({delta:+}, {percent:+.1}%)",
                        want.entries, got.entries
                    ),
                ));
            }
            (want, got) => drifted.push((
                i128::MAX,
                format!("{key}: outcome changed\n    was: {}\n    now: {}", want.render(), got.render()),
            )),
        }
    }

    drifted.sort_by(|a, b| b.0.cmp(&a.0));
    assert!(
        drifted.is_empty(),
        "{} of {} measured corpus package version(s) no longer match \
         corpus/entry-baseline.toml, largest change first:\n{}\n\n\
         The corpus fixtures are sha256-pinned by corpus/manifest.toml, so the input \
         did not move — this is a change in our own code. Decide whether it is an \
         improvement before recording it, then:\n  \
         UPDATE_CORPUS_BASELINE=1 cargo test -p nudox-store --features fixtures \
         --test corpus_contract corpus_entry_counts -- --ignored --nocapture",
        drifted.len(),
        measured_rows.len(),
        drifted
            .iter()
            .map(|(_, line)| format!("  {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

/// Which ecosystems a regeneration run should re-measure, from
/// `NUDOX_BASELINE_ECOSYSTEMS` (comma-separated tokens). `None` means all.
///
/// An unrecognised token panics rather than being skipped: a typo that silently
/// measured nothing would produce a baseline file full of carried-forward rows
/// and look like a successful regeneration.
fn measurement_scope() -> Option<Vec<baseline::Ecosystem>> {
    let raw = std::env::var("NUDOX_BASELINE_ECOSYSTEMS").ok()?;
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let all = [
        baseline::Ecosystem::CratesIo,
        baseline::Ecosystem::Go,
        baseline::Ecosystem::Maven,
        baseline::Ecosystem::Nuget,
        baseline::Ecosystem::Npm,
        baseline::Ecosystem::Pypi,
        baseline::Ecosystem::Cpp,
    ];
    Some(
        raw.split(',')
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(|token| {
                *all.iter()
                    .find(|eco| eco.token() == token)
                    .unwrap_or_else(|| {
                        panic!(
                            "NUDOX_BASELINE_ECOSYSTEMS: unknown ecosystem {token:?}; \
                             expected one of {}",
                            all.iter().map(|e| e.token()).collect::<Vec<_>>().join(", ")
                        )
                    })
            })
            .collect(),
    )
}

/// Run the registered producer for one corpus package and record what happened.
///
/// Goes through `ProducerSource`/`IrSource` — the same seam the engine loads a
/// workspace through — rather than calling a concrete producer directly, so the
/// numbers this file pins are the numbers the product actually gets. A producer
/// failure is a recorded outcome, not a panic: one unbuildable package must not
/// stop the other 153 from being measured.
fn measure_one(
    runtime: &tokio::runtime::Runtime,
    key: &baseline::CorpusKey,
    dir: &std::path::Path,
) -> baseline::VersionBaseline {
    use nudox_store::source::producer::{ProducerRegistry, ProducerSource};

    let descriptor = key.ecosystem.descriptor_for(dir, &key.name, &key.version);
    let source = ProducerSource::new(
        std::sync::Arc::new(ProducerRegistry::with_all_available()),
        vec![descriptor],
    );

    runtime.block_on(async move {
        let mut stream = source.load(LoadRequest::default());
        let mut outcome: Option<baseline::VersionBaseline> = None;
        while let Some(event) = stream.next().await {
            match event {
                Ok(LoadEvent::Ready { package }) => {
                    outcome = Some(baseline::VersionBaseline::Lowered(baseline::LoweredRow {
                        version: key.version.clone(),
                        entries: package.view().table().len(),
                    }));
                }
                Ok(LoadEvent::Failed { error, .. }) => {
                    outcome = Some(baseline::VersionBaseline::Failed(baseline::FailedRow {
                        version: key.version.clone(),
                        producer_error: baseline::escape(&error.to_string()),
                    }));
                }
                Ok(_) => {}
                Err(error) => {
                    outcome = Some(baseline::VersionBaseline::Failed(baseline::FailedRow {
                        version: key.version.clone(),
                        producer_error: baseline::escape(&format!("stream error: {error}")),
                    }));
                }
            }
        }
        // A source that emitted neither Ready nor Failed produced no evidence
        // either way. Recording that as a zero count would be a fabricated
        // measurement, so it is recorded as the failure it is.
        outcome.unwrap_or(baseline::VersionBaseline::Failed(baseline::FailedRow {
            version: key.version.clone(),
            producer_error: "producer stream ended with neither Ready nor Failed".to_owned(),
        }))
    })
}
