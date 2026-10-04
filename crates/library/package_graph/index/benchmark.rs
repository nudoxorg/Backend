//! Ignored synthetic A/B benchmark for the private flat ordinal experiment.
//!
//! The fixture is generated in memory, not acquired from a registry or a
//! retained production snapshot. Output is JSONL and records distributions;
//! this module makes no claim until a reviewer grants a run slot and captures
//! the output with the toolchain/host receipt.

use super::*;
use allocation_counter::measure;
use std::{collections::BTreeMap, hint::black_box, time::Instant};

const SOURCE_COUNT: usize = 2_048;
const MAX_REVERSE_QUERIES: usize = 64;
const BUILD_TRIALS: usize = 15;
const FIRST_QUERY_TRIALS: usize = 31;
const QUERY_ROUNDS: usize = 11;

struct MapCandidate {
    checked: CheckedPackageGraphFacts,
    index: PackageGraphIndex,
}

impl MapCandidate {
    fn from_checked(checked: CheckedPackageGraphFacts) -> Self {
        let index = PackageGraphIndex::from_facts(checked.facts());
        Self { checked, index }
    }

    fn from_borrowed<'a, I>(
        facts: I,
        limits: PackageGraphIndexLimits,
    ) -> Result<Self, PackageGraphAdmissionError>
    where
        I: Iterator<Item = &'a PackageDependencySourceFacts> + Clone,
    {
        let checked = CheckedPackageGraphFacts::from_borrowed_facts(facts, limits)?;
        Ok(Self::from_checked(checked))
    }

    fn copied_lookup_text_bytes(&self) -> usize {
        let source_key_bytes = self
            .index
            .by_source
            .keys()
            .map(|source| source.coordinate.as_str().len())
            .sum::<usize>();
        let coordinate_key_bytes = self
            .index
            .by_coordinate
            .keys()
            .map(|coordinate| coordinate.as_str().len())
            .sum::<usize>();
        let reverse_key_bytes = self
            .index
            .reverse
            .values()
            .flat_map(|names| names.iter())
            .map(|(name, edges)| {
                name.len().saturating_add(
                    edges
                        .resolved
                        .keys()
                        .map(|reference| reference.as_str().len())
                        .sum::<usize>(),
                )
            })
            .sum::<usize>();
        source_key_bytes
            .saturating_add(coordinate_key_bytes)
            .saturating_add(reverse_key_bytes)
    }
}

trait Candidate {
    fn witness(&self) -> [u8; 32];
    fn dependencies(&self, package: &PackageReference) -> PackageDependencyLookup<'_>;
    fn dependencies_for_source(
        &self,
        source: &PackageGraphSourceKey,
    ) -> Option<&DependencyFacts<Box<[PackageDependencyRecord]>>>;
    fn dependent_sources(&self, package: &PackageReference) -> DependentSources;
    fn dependent_edges_page(
        &self,
        package: &PackageReference,
        after: Option<[u8; 32]>,
        limit: u16,
    ) -> (Vec<&PackageDependencyRecord>, bool);
}

impl Candidate for MapCandidate {
    fn witness(&self) -> [u8; 32] {
        self.checked.witness()
    }

    fn dependencies(&self, package: &PackageReference) -> PackageDependencyLookup<'_> {
        self.index.dependencies(self.facts(), package)
    }

    fn dependencies_for_source(
        &self,
        source: &PackageGraphSourceKey,
    ) -> Option<&DependencyFacts<Box<[PackageDependencyRecord]>>> {
        self.index.dependencies_for_source(self.facts(), source)
    }

    fn dependent_sources(&self, package: &PackageReference) -> DependentSources {
        self.index.dependent_sources(self.facts(), package)
    }

    fn dependent_edges_page(
        &self,
        package: &PackageReference,
        after: Option<[u8; 32]>,
        limit: u16,
    ) -> (Vec<&PackageDependencyRecord>, bool) {
        self.index
            .dependent_edges_page(self.facts(), package, after, limit)
    }
}

impl Candidate for FlatCheckedGraph {
    fn witness(&self) -> [u8; 32] {
        FlatCheckedGraph::witness(self)
    }

    fn dependencies(&self, package: &PackageReference) -> PackageDependencyLookup<'_> {
        FlatCheckedGraph::dependencies(self, package)
    }

    fn dependencies_for_source(
        &self,
        source: &PackageGraphSourceKey,
    ) -> Option<&DependencyFacts<Box<[PackageDependencyRecord]>>> {
        FlatCheckedGraph::dependencies_for_source(self, source)
    }

    fn dependent_sources(&self, package: &PackageReference) -> DependentSources {
        FlatCheckedGraph::dependent_sources(self, package)
    }

    fn dependent_edges_page(
        &self,
        package: &PackageReference,
        after: Option<[u8; 32]>,
        limit: u16,
    ) -> (Vec<&PackageDependencyRecord>, bool) {
        FlatCheckedGraph::dependent_edges_page(self, package, after, limit)
    }
}

enum Query {
    Coordinate(PackageReference),
    ExactSource(PackageGraphSourceKey),
    ReverseSources(PackageReference),
    ReversePage(PackageReference, u16),
}

#[derive(Debug, Eq, PartialEq)]
enum QueryResult<'a> {
    Coordinate(PackageDependencyLookup<'a>),
    ExactSource(Option<&'a DependencyFacts<Box<[PackageDependencyRecord]>>>),
    ReverseSources(DependentSources),
    ReversePage(Vec<&'a PackageDependencyRecord>, bool),
}

impl Query {
    const fn class(&self) -> &'static str {
        match self {
            Self::Coordinate(_) => "forward_coordinate",
            Self::ExactSource(_) => "forward_exact_source",
            Self::ReverseSources(_) => "reverse_sources",
            Self::ReversePage(_, _) => "reverse_page",
        }
    }
}

struct TimingSample {
    phase: &'static str,
    candidate: &'static str,
    query_class: Option<&'static str>,
    query_slot: Option<usize>,
    trial: usize,
    elapsed_ns: u128,
}

fn limits() -> PackageGraphIndexLimits {
    PackageGraphIndexLimits {
        max_sources: 4_096,
        max_fact_bytes: 512 * 1024 * 1024,
        max_total_rows: 1_000_000,
        max_reverse_edges: 1_000_000,
        max_index_key_bytes: 512 * 1024 * 1024,
    }
}

fn build_queries(facts: &[PackageDependencySourceFacts]) -> Vec<Query> {
    let mut queries = Vec::new();
    for (source, _) in facts.iter().take(48) {
        queries.push(Query::Coordinate(source.coordinate.clone()));
        queries.push(Query::ExactSource(source.clone()));
    }

    let typed_collision_purl = PackageReference::parse("pkg:cargo/benchmark-kind-collision@1.0.0")
        .expect("PURL collision coordinate");
    let typed_collision_local = PackageReference::from_kind(
        crate::PackageReferenceKind::Local,
        typed_collision_purl.as_str(),
    )
    .expect("same-spelling local collision label");
    for (coordinate, authority) in [
        (
            typed_collision_purl.clone(),
            PackageGraphSourceAuthority::Unattributed,
        ),
        (
            typed_collision_local.clone(),
            PackageGraphSourceAuthority::Unattributed,
        ),
    ] {
        queries.push(Query::Coordinate(coordinate.clone()));
        queries.push(Query::ExactSource(PackageGraphSourceKey::new(
            coordinate, authority,
        )));
    }
    queries.push(Query::ReverseSources(typed_collision_purl.clone()));
    queries.push(Query::ReversePage(typed_collision_purl.clone(), 1));
    queries.push(Query::ReverseSources(typed_collision_local.clone()));
    queries.push(Query::ReversePage(typed_collision_local, 1));

    let fanout =
        PackageReference::parse("pkg:cargo/benchmark-fanout@1.0.0").expect("fanout package");
    queries.push(Query::Coordinate(fanout));
    queries.push(Query::Coordinate(
        PackageReference::parse("pkg:cargo/benchmark-missing@1.0.0").expect("missing package"),
    ));
    let long_name = format!("long-{}", "segment-".repeat(12));
    queries.push(Query::Coordinate(
        PackageReference::parse(format!("pkg:cargo/{long_name}@1.0.0"))
            .expect("long missing package"),
    ));
    queries.push(Query::ReversePage(
        PackageReference::parse("pkg:cargo/benchmark-missing@1.0.0")
            .expect("missing reverse package"),
        128,
    ));
    queries.push(Query::ExactSource(PackageGraphSourceKey::new(
        PackageReference::parse("pkg:cargo/benchmark-missing@1.0.0")
            .expect("missing source package"),
        PackageGraphSourceAuthority::Registry(RegistryAuthorityId::from_configured_source(
            [0xa5; 32],
        )),
    )));

    let mut reverse_count = 0;
    for (_, state) in facts {
        let DependencyFacts::Known(rows) = state else {
            continue;
        };
        for row in rows {
            if !counts_for_reverse(row) {
                continue;
            }
            let ecosystem = match row.target.ecosystem {
                RegistryEcosystem::Cargo => "cargo",
                RegistryEcosystem::Npm => "npm",
                RegistryEcosystem::Pypi => "pypi",
                other => panic!("synthetic reverse query has unsupported ecosystem {other:?}"),
            };
            let lineage = PackageReference::parse(&format!(
                "pkg:{ecosystem}/{}@1.0.0",
                row.target.name.as_str()
            ))
            .expect("lineage query");
            queries.push(Query::ReverseSources(lineage.clone()));
            queries.push(Query::ReversePage(lineage, 1));
            if let Some(resolved) = &row.target.resolved {
                queries.push(Query::ReversePage(resolved.clone(), 128));
            }
            reverse_count += 1;
            if reverse_count == MAX_REVERSE_QUERIES {
                return queries;
            }
        }
    }
    queries
}

fn representative_query_slots(queries: &[Query]) -> Vec<usize> {
    [
        "forward_coordinate",
        "forward_exact_source",
        "reverse_sources",
        "reverse_page",
    ]
    .into_iter()
    .filter_map(|class| queries.iter().position(|query| query.class() == class))
    .collect()
}

fn oracle_lookup<'a>(
    facts: &'a [PackageDependencySourceFacts],
    package: &PackageReference,
) -> PackageDependencyLookup<'a> {
    let matches = facts
        .iter()
        .filter(|(source, _)| &source.coordinate == package)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => PackageDependencyLookup::Missing,
        [(source, state)] => PackageDependencyLookup::Exact {
            source,
            facts: state,
        },
        many => PackageDependencyLookup::Ambiguous(
            many.iter()
                .map(|(source, _)| (*source).clone())
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        ),
    }
}

fn oracle_query<'a>(facts: &'a [PackageDependencySourceFacts], query: &Query) -> QueryResult<'a> {
    match query {
        Query::Coordinate(package) => QueryResult::Coordinate(oracle_lookup(facts, package)),
        Query::ExactSource(source) => QueryResult::ExactSource(
            facts
                .iter()
                .find(|(candidate, _)| candidate == source)
                .map(|(_, state)| state),
        ),
        Query::ReverseSources(package) => {
            QueryResult::ReverseSources(linear_dependent_sources(facts, package))
        }
        Query::ReversePage(package, limit) => {
            let (rows, more) = tests::scan_page(facts, package, None, *limit);
            QueryResult::ReversePage(rows, more)
        }
    }
}

fn execute_query<'a, G: Candidate>(graph: &'a G, query: &Query) -> QueryResult<'a> {
    match query {
        Query::Coordinate(package) => QueryResult::Coordinate(graph.dependencies(package)),
        Query::ExactSource(source) => {
            QueryResult::ExactSource(graph.dependencies_for_source(source))
        }
        Query::ReverseSources(package) => {
            QueryResult::ReverseSources(graph.dependent_sources(package))
        }
        Query::ReversePage(package, limit) => {
            let (rows, more) = graph.dependent_edges_page(package, None, *limit);
            QueryResult::ReversePage(rows, more)
        }
    }
}

fn output_cardinality(result: &QueryResult<'_>) -> usize {
    fn state_cardinality(state: &DependencyFacts<Box<[PackageDependencyRecord]>>) -> usize {
        match state {
            DependencyFacts::Known(rows) => rows.len(),
            DependencyFacts::Unknown(_) | DependencyFacts::Unavailable(_) => 1,
        }
    }

    match result {
        QueryResult::Coordinate(PackageDependencyLookup::Missing) => 0,
        QueryResult::Coordinate(PackageDependencyLookup::Exact { facts, .. }) => {
            state_cardinality(facts)
        }
        QueryResult::Coordinate(PackageDependencyLookup::Ambiguous(sources)) => sources.len(),
        QueryResult::ExactSource(None) => 0,
        QueryResult::ExactSource(Some(state)) => state_cardinality(state),
        QueryResult::ReverseSources(DependentSources::NotPurl) => 0,
        QueryResult::ReverseSources(DependentSources::Matched { sources, gap }) => {
            sources.len().saturating_add(usize::from(gap.is_some()))
        }
        QueryResult::ReversePage(rows, more) => rows.len().saturating_add(usize::from(*more)),
    }
}

fn time_candidate_query<G: Candidate>(graph: &G, query: &Query) -> u128 {
    let graph = black_box(graph);
    let query = black_box(query);
    let start = Instant::now();
    let result = execute_query(graph, query);
    let elapsed = start.elapsed().as_nanos();
    black_box(output_cardinality(&result));
    black_box(result);
    elapsed
}

fn retain_verified_witness<G: Candidate>(graph: &G, expected: [u8; 32]) {
    let observed = graph.witness();
    assert_eq!(
        observed, expected,
        "candidate changed the checked facts witness"
    );
    black_box(observed);
}

fn hex(value: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(value.len().saturating_mul(2));
    for byte in value {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn percentile(sorted: &[u128], percentile: usize) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = sorted.len().saturating_mul(percentile).saturating_add(99) / 100;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn emit_results(samples: &[TimingSample], metadata: &serde_json::Value) {
    let mut grouped =
        BTreeMap::<(&'static str, &'static str, Option<&'static str>), Vec<u128>>::new();
    for sample in samples {
        println!(
            "{}",
            serde_json::json!({
                "kind": "raw",
                "phase": sample.phase,
                "candidate": sample.candidate,
                "query_class": sample.query_class,
                "query_slot": sample.query_slot,
                "trial": sample.trial,
                "elapsed_ns": sample.elapsed_ns,
                "dataset": metadata,
            })
        );
        grouped
            .entry((sample.phase, sample.candidate, sample.query_class))
            .or_default()
            .push(sample.elapsed_ns);
    }
    for ((phase, candidate, query_class), mut values) in grouped {
        values.sort_unstable();
        println!(
            "{}",
            serde_json::json!({
                "kind": "summary",
                "phase": phase,
                "candidate": candidate,
                "query_class": query_class,
                "sample_unit": match (phase, query_class.is_some()) {
                    ("warm_query", true) => {
                        "one query call after every candidate query was primed; result cardinality barrier is outside elapsed interval"
                    }
                    ("first_query_after_build", true) => {
                        "one first query call on a newly constructed candidate; candidate build and result cardinality barrier are outside elapsed interval; process caches are not claimed cold"
                    }
                    ("build_from_checked_snapshot", false) => {
                        "index construction from prechecked immutable facts"
                    }
                    ("borrowed_admission_plus_build", false) => {
                        "borrowed admission, checked-fact construction, and index build"
                    }
                    _ => "unexpected sample grouping",
                },
                "samples": values.len(),
                "median_ns": percentile(&values, 50),
                "p95_ns": percentile(&values, 95),
                "p99_ns": percentile(&values, 99),
                "dataset": metadata,
            })
        );
    }
}

#[test]
#[ignore = "synthetic comparison only; run after review and record the host/toolchain receipt"]
fn ignored_flat_ordinal_ab_benchmark_emits_raw_jsonl_and_distributions() {
    let seed = 0x5eed_cafe_d15c_a11u64;
    let (mut facts, _) = tests::generated_facts(seed, SOURCE_COUNT);
    facts.extend(tests::typed_identity_collision_facts());
    let fanout_coordinate =
        PackageReference::parse("pkg:cargo/benchmark-fanout@1.0.0").expect("fanout package");
    for authority in 1..=64_u8 {
        facts.push((
            PackageGraphSourceKey::new(
                fanout_coordinate.clone(),
                PackageGraphSourceAuthority::Registry(RegistryAuthorityId::from_configured_source(
                    [authority; 32],
                )),
            ),
            DependencyFacts::Known(Vec::new().into_boxed_slice()),
        ));
    }
    let limits = limits();
    let checked = CheckedPackageGraphFacts::new_with_limits(facts.clone(), limits)
        .expect("precomputed synthetic checked snapshot");
    let expected_witness = checked.witness();
    let queries = build_queries(checked.facts());
    assert!(
        !queries.is_empty(),
        "precomputed query corpus must be nonempty"
    );
    let first_query_slots = representative_query_slots(&queries);
    assert_eq!(
        first_query_slots.len(),
        4,
        "the fixed corpus must contain one representative of every query class"
    );

    let baseline = MapCandidate::from_checked(checked.clone());
    let flat = FlatCheckedGraph::from_checked(checked.clone()).expect("flat index build");
    for query in &queries {
        let oracle = oracle_query(checked.facts(), query);
        assert_eq!(
            execute_query(&baseline, query),
            oracle,
            "map oracle: {}",
            query.class()
        );
        assert_eq!(
            execute_query(&flat, query),
            oracle,
            "flat oracle: {}",
            query.class()
        );
    }

    let flat_posting_count = flat.reverse_posting_count();
    let flat_posting_capacity = flat.reverse_posting_capacity();
    let flat_posting_buffer_requested_bytes = flat.posting_buffer_requested_bytes();
    let flat_index_layout_bytes = flat.index_layout_bytes_including_vec_header();
    let map_copied_lookup_text_bytes = baseline.copied_lookup_text_bytes();
    drop(flat);
    drop(baseline);

    let query_class_counts = [
        "forward_coordinate",
        "forward_exact_source",
        "reverse_sources",
        "reverse_page",
    ]
    .into_iter()
    .map(|class| {
        (
            class,
            queries
                .iter()
                .filter(|query| query.class() == class)
                .count(),
        )
    })
    .collect::<BTreeMap<_, _>>();
    let first_query_descriptions = first_query_slots
        .iter()
        .map(|query_slot| {
            serde_json::json!({
                "query_slot": query_slot,
                "query_class": queries[*query_slot].class(),
            })
        })
        .collect::<Vec<_>>();
    let metadata = serde_json::json!({
        "dataset_class": "synthetic_generated_facts_with_typed_identity_fixture_v2",
        "seed": format!("0x{seed:016x}"),
        "generated_source_count": SOURCE_COUNT,
        "typed_identity_fixture_source_count": 3,
        "typed_identity_fixture": "PURL and Local source coordinates share display text; a single admitted resolved edge targets only the PURL identity",
        "authority_fanout_fixture_source_count": 64,
        "sources": checked.facts().len(),
        "known_rows": checked.facts().iter().map(|(_, state)| match state {
            DependencyFacts::Known(rows) => rows.len(),
            DependencyFacts::Unknown(_) | DependencyFacts::Unavailable(_) => 0,
        }).sum::<usize>(),
        "unknown_sources": checked.facts().iter().filter(|(_, state)| matches!(state, DependencyFacts::Unknown(_))).count(),
        "unavailable_sources": checked.facts().iter().filter(|(_, state)| matches!(state, DependencyFacts::Unavailable(_))).count(),
        "known_empty_sources": checked.facts().iter().filter(|(_, state)| matches!(state, DependencyFacts::Known(rows) if rows.is_empty())).count(),
        "query_count": queries.len(),
        "query_class_counts": query_class_counts,
        "first_query_slots": first_query_descriptions,
        "build_trials_per_candidate": BUILD_TRIALS,
        "first_query_trials_per_class_and_candidate": FIRST_QUERY_TRIALS,
        "warm_query_rounds": QUERY_ROUNDS,
        "pair_order_policy": "build/admission alternates candidate order by trial parity; first-query alternates by trial plus representative-slot ordinal; warm-query alternates by round plus query-slot parity",
        "query_cache_note": "first_query_after_build excludes candidate construction but is not a process-cold or hardware-cache-cold measurement; warm_query primes each query once on each persistent candidate before timed rounds",
        "limits": {
            "max_sources": limits.max_sources,
            "max_fact_bytes": limits.max_fact_bytes,
            "max_total_rows": limits.max_total_rows,
            "max_reverse_edges": limits.max_reverse_edges,
            "max_index_key_bytes": limits.max_index_key_bytes,
        },
        "facts_witness": hex(&checked.witness()),
        "host_os": std::env::consts::OS,
        "host_arch": std::env::consts::ARCH,
        "posting_width_bytes": std::mem::size_of::<ReversePosting>(),
        "flat_posting_count": flat_posting_count,
        "flat_posting_capacity": flat_posting_capacity,
        "flat_posting_buffer_requested_bytes": flat_posting_buffer_requested_bytes,
        "flat_index_layout_bytes_including_vec_header": flat_index_layout_bytes,
        "map_copied_lookup_text_bytes": map_copied_lookup_text_bytes,
        "flat_copied_lookup_text_bytes": 0,
        "memory_note": "posting-buffer requested bytes and copied key-text payload only; not allocator overhead, retained heap, or RSS",
    });
    println!(
        "{}",
        serde_json::json!({ "kind": "dataset", "metadata": &metadata })
    );

    let mut samples = Vec::new();
    for trial in 0..BUILD_TRIALS {
        if trial % 2 == 0 {
            let start = Instant::now();
            let graph = MapCandidate::from_checked(checked.clone());
            let elapsed_ns = start.elapsed().as_nanos();
            retain_verified_witness(&graph, expected_witness);
            samples.push(TimingSample {
                phase: "build_from_checked_snapshot",
                candidate: "map_baseline",
                query_class: None,
                query_slot: None,
                trial,
                elapsed_ns,
            });
            drop(graph);

            let start = Instant::now();
            let graph = FlatCheckedGraph::from_checked(checked.clone()).expect("flat build");
            let elapsed_ns = start.elapsed().as_nanos();
            retain_verified_witness(&graph, expected_witness);
            samples.push(TimingSample {
                phase: "build_from_checked_snapshot",
                candidate: "flat_ordinals",
                query_class: None,
                query_slot: None,
                trial,
                elapsed_ns,
            });
            drop(graph);
        } else {
            let start = Instant::now();
            let graph = FlatCheckedGraph::from_checked(checked.clone()).expect("flat build");
            let elapsed_ns = start.elapsed().as_nanos();
            retain_verified_witness(&graph, expected_witness);
            samples.push(TimingSample {
                phase: "build_from_checked_snapshot",
                candidate: "flat_ordinals",
                query_class: None,
                query_slot: None,
                trial,
                elapsed_ns,
            });
            drop(graph);

            let start = Instant::now();
            let graph = MapCandidate::from_checked(checked.clone());
            let elapsed_ns = start.elapsed().as_nanos();
            retain_verified_witness(&graph, expected_witness);
            samples.push(TimingSample {
                phase: "build_from_checked_snapshot",
                candidate: "map_baseline",
                query_class: None,
                query_slot: None,
                trial,
                elapsed_ns,
            });
            drop(graph);
        }
    }

    for trial in 0..BUILD_TRIALS {
        if trial % 2 == 0 {
            let start = Instant::now();
            let graph = MapCandidate::from_borrowed(facts.iter(), limits)
                .expect("map full admission and build");
            let elapsed_ns = start.elapsed().as_nanos();
            retain_verified_witness(&graph, expected_witness);
            samples.push(TimingSample {
                phase: "borrowed_admission_plus_build",
                candidate: "map_baseline",
                query_class: None,
                query_slot: None,
                trial,
                elapsed_ns,
            });
            drop(graph);

            let start = Instant::now();
            let graph = FlatCheckedGraph::from_borrowed_facts(facts.iter(), limits)
                .expect("flat full admission and build");
            let elapsed_ns = start.elapsed().as_nanos();
            retain_verified_witness(&graph, expected_witness);
            samples.push(TimingSample {
                phase: "borrowed_admission_plus_build",
                candidate: "flat_ordinals",
                query_class: None,
                query_slot: None,
                trial,
                elapsed_ns,
            });
            drop(graph);
        } else {
            let start = Instant::now();
            let graph = FlatCheckedGraph::from_borrowed_facts(facts.iter(), limits)
                .expect("flat full admission and build");
            let elapsed_ns = start.elapsed().as_nanos();
            retain_verified_witness(&graph, expected_witness);
            samples.push(TimingSample {
                phase: "borrowed_admission_plus_build",
                candidate: "flat_ordinals",
                query_class: None,
                query_slot: None,
                trial,
                elapsed_ns,
            });
            drop(graph);

            let start = Instant::now();
            let graph = MapCandidate::from_borrowed(facts.iter(), limits)
                .expect("map full admission and build");
            let elapsed_ns = start.elapsed().as_nanos();
            retain_verified_witness(&graph, expected_witness);
            samples.push(TimingSample {
                phase: "borrowed_admission_plus_build",
                candidate: "map_baseline",
                query_class: None,
                query_slot: None,
                trial,
                elapsed_ns,
            });
            drop(graph);
        }
    }

    for trial in 0..FIRST_QUERY_TRIALS {
        for (representative_ordinal, &query_slot) in first_query_slots.iter().enumerate() {
            let query = &queries[query_slot];
            if (trial + representative_ordinal) % 2 == 0 {
                let graph = MapCandidate::from_checked(checked.clone());
                retain_verified_witness(&graph, expected_witness);
                samples.push(TimingSample {
                    phase: "first_query_after_build",
                    candidate: "map_baseline",
                    query_class: Some(query.class()),
                    query_slot: Some(query_slot),
                    trial,
                    elapsed_ns: time_candidate_query(&graph, query),
                });
                drop(graph);

                let graph = FlatCheckedGraph::from_checked(checked.clone()).expect("flat build");
                retain_verified_witness(&graph, expected_witness);
                samples.push(TimingSample {
                    phase: "first_query_after_build",
                    candidate: "flat_ordinals",
                    query_class: Some(query.class()),
                    query_slot: Some(query_slot),
                    trial,
                    elapsed_ns: time_candidate_query(&graph, query),
                });
                drop(graph);
            } else {
                let graph = FlatCheckedGraph::from_checked(checked.clone()).expect("flat build");
                retain_verified_witness(&graph, expected_witness);
                samples.push(TimingSample {
                    phase: "first_query_after_build",
                    candidate: "flat_ordinals",
                    query_class: Some(query.class()),
                    query_slot: Some(query_slot),
                    trial,
                    elapsed_ns: time_candidate_query(&graph, query),
                });
                drop(graph);

                let graph = MapCandidate::from_checked(checked.clone());
                retain_verified_witness(&graph, expected_witness);
                samples.push(TimingSample {
                    phase: "first_query_after_build",
                    candidate: "map_baseline",
                    query_class: Some(query.class()),
                    query_slot: Some(query_slot),
                    trial,
                    elapsed_ns: time_candidate_query(&graph, query),
                });
                drop(graph);
            }
        }
    }

    let query_baseline = MapCandidate::from_checked(checked.clone());
    let query_flat = FlatCheckedGraph::from_checked(checked.clone()).expect("query flat index");
    for query in &queries {
        black_box(execute_query(&query_baseline, query));
        black_box(execute_query(&query_flat, query));
    }
    for round in 0..QUERY_ROUNDS {
        for (query_index, query) in queries.iter().enumerate() {
            if (round + query_index) % 2 == 0 {
                samples.push(TimingSample {
                    phase: "warm_query",
                    candidate: "map_baseline",
                    query_class: Some(query.class()),
                    query_slot: Some(query_index),
                    trial: round,
                    elapsed_ns: time_candidate_query(&query_baseline, query),
                });
                samples.push(TimingSample {
                    phase: "warm_query",
                    candidate: "flat_ordinals",
                    query_class: Some(query.class()),
                    query_slot: Some(query_index),
                    trial: round,
                    elapsed_ns: time_candidate_query(&query_flat, query),
                });
            } else {
                samples.push(TimingSample {
                    phase: "warm_query",
                    candidate: "flat_ordinals",
                    query_class: Some(query.class()),
                    query_slot: Some(query_index),
                    trial: round,
                    elapsed_ns: time_candidate_query(&query_flat, query),
                });
                samples.push(TimingSample {
                    phase: "warm_query",
                    candidate: "map_baseline",
                    query_class: Some(query.class()),
                    query_slot: Some(query_index),
                    trial: round,
                    elapsed_ns: time_candidate_query(&query_baseline, query),
                });
            }
        }
    }

    // Allocation samples are intentionally separate from timed samples. The
    // counter reports allocator events on the measured test thread; it is not
    // an RSS or retained-heap measurement.
    drop(query_baseline);
    drop(query_flat);
    let mut measured_map = None;
    let map_allocations = measure(|| {
        measured_map = Some(MapCandidate::from_checked(checked.clone()));
    });
    black_box(measured_map.as_ref().map(|graph| graph.witness()));
    drop(measured_map);
    let mut measured_flat = None;
    let flat_allocations = measure(|| {
        measured_flat =
            Some(FlatCheckedGraph::from_checked(checked.clone()).expect("flat allocation sample"));
    });
    black_box(measured_flat.as_ref().map(|graph| graph.witness()));
    drop(measured_flat);
    println!(
        "{}",
        serde_json::json!({
            "kind": "allocation_sample",
            "measurement": "allocation_counter::measure on the test thread; not RSS or retained heap",
            "scope": "checked snapshot clone plus candidate index construction; candidate drop is outside the measured closure",
            "map_baseline": format!("{map_allocations:?}"),
            "flat_ordinals": format!("{flat_allocations:?}"),
            "dataset": &metadata,
        })
    );

    emit_results(&samples, &metadata);
}
