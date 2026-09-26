//! Reverse package adjacency against a full fact scan.
//!
//! Fixture construction is outside every timer. `cold` builds the index and
//! answers one dependent query. `warm` answers from an index that already
//! exists. `scan` walks every edge for the same query.

#![allow(clippy::expect_used, clippy::print_stdout)]

use std::time::Instant;

use backend_library::{
    DependencyAuthority, DependencyEvidence, DependencyFacts, DependencyScope,
    PackageDependencyRecord, PackageDependencySourceFacts, PackageDependencyTarget,
    PackageGraphIndex, PackageReference, RegistryEcosystem, linear_dependent_sources,
};

const SOURCES: usize = 4_096;
const EDGES: usize = 8;
const SAMPLES: usize = 32;
const WARMUPS: usize = 4;

fn main() {
    let facts = fixture(SOURCES, EDGES);
    let target = PackageReference::parse("pkg:cargo/target-lib@1.0.0").expect("target");
    let index = PackageGraphIndex::from_facts(&facts);
    let indexed = index.dependent_sources(&facts, &target);
    let scanned = linear_dependent_sources(&facts, &target);
    if indexed != scanned {
        eprintln!("package_graph index diverged from the linear scan");
        std::process::exit(1);
    }
    let matched = match &indexed {
        backend_library::DependentSources::Matched { sources, .. } => sources.len(),
        backend_library::DependentSources::NotPurl => {
            eprintln!("package_graph fixture target was not a package URL");
            std::process::exit(1);
        }
    };
    let cold = time(|| {
        let built = PackageGraphIndex::from_facts(&facts);
        built.dependent_sources(&facts, &target)
    });
    let warm = time(|| index.dependent_sources(&facts, &target));
    let scan = time(|| linear_dependent_sources(&facts, &target));
    let (cold_median, cold_p95) = percentiles(&cold);
    let (warm_median, warm_p95) = percentiles(&warm);
    let (scan_median, scan_p95) = percentiles(&scan);
    println!(
        "package_graph sources={SOURCES} edges={} matched={matched} cold_median_ns={cold_median} cold_p95_ns={cold_p95} warm_median_ns={warm_median} warm_p95_ns={warm_p95} scan_median_ns={scan_median} scan_p95_ns={scan_p95}",
        SOURCES * EDGES
    );
}

fn time<T>(mut body: impl FnMut() -> T) -> Vec<u128> {
    for _ in 0..WARMUPS {
        let _ = body();
    }
    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let started = Instant::now();
        let _ = body();
        samples.push(started.elapsed().as_nanos());
    }
    samples
}

fn percentiles(samples: &[u128]) -> (u128, u128) {
    let mut ordered = samples.to_vec();
    ordered.sort_unstable();
    let median = ordered[ordered.len() / 2];
    let p95 = ordered[ordered.len() * 95 / 100];
    (median, p95)
}

fn fixture(sources: usize, edges: usize) -> Vec<PackageDependencySourceFacts> {
    let mut facts = Vec::with_capacity(sources);
    for source_index in 0..sources {
        let source = PackageReference::parse(format!("pkg:cargo/source-{source_index}@1.0.0"))
            .expect("source");
        let mut rows = Vec::with_capacity(edges);
        for edge in 0..edges {
            let (name, resolved, scope) = if source_index < 4 && edge == 0 {
                (
                    "target-lib",
                    Some(PackageReference::parse("pkg:cargo/target-lib@1.0.0").expect("target")),
                    DependencyScope::Runtime,
                )
            } else if source_index == 5 && edge == 0 {
                ("target-lib", None, DependencyScope::Development)
            } else if source_index == 6 && edge == 0 {
                ("target-lib", None, DependencyScope::Optional)
            } else {
                ("other-lib", None, DependencyScope::Runtime)
            };
            rows.push(PackageDependencyRecord::new(
                source.clone(),
                PackageDependencyTarget::new(RegistryEcosystem::Cargo, name, "^1", resolved)
                    .expect("target"),
                scope,
                false,
                DependencyEvidence {
                    authority: DependencyAuthority::RegistryMetadata,
                    frontier: [u8::try_from(edge).unwrap_or(0); 32],
                    provenance: [u8::try_from(source_index).unwrap_or(0); 32],
                },
            ));
        }
        facts.push((source, DependencyFacts::Known(rows.into_boxed_slice())));
    }
    facts
}
