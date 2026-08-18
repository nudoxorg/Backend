//! Symbol-identity disambiguator-tier census over the real crates.io corpus.
//!
//! # Why this exists
//!
//! `IntroId` identity depends on a per-declaration [`Disambiguator`]
//! (`nudox_ir::intro`), escalated at `workspace/ir/model/src/package/seal.rs:420-543`
//! when the structural choice (`None` / `FnOverload` / `TraitImpl`) collides.
//! `KeyTier` (`seal.rs:229-259`) collapses that five-way choice into three
//! buckets — `Structural | Span | Ordinal` — for callers that just need "is
//! this key content-derived". A design decision about giving `Param`/`Field`
//! a positional disambiguator needs the five-way split back, split further
//! into the buckets a path-addressing scheme actually cares about:
//!
//! * `f_none`     — `Disambiguator::None`: addressable by path today, zero
//!   new seal output required.
//! * `f_struct`   — `FnOverload` | `TraitImpl`: addressable only once the
//!   sealer renders the disambiguator's skeleton to text.
//! * `f_unstable` — `Span` | `Ordinal`: never path-addressable; a caller must
//!   hash forever.
//!
//! # `SealReport::forced_keys` cannot answer this
//!
//! `forced_keys` (the field `nudox_store`/`nudox-engine`'s `KeyProvenance`
//! actually reads, `workspace/nudox-engine/src/store/package.rs:493-514`)
//! only names declarations pass 2.5 **escalated**. Pass 2's own "other
//! collision" arm (`seal.rs` around line 403) can mint `Disambiguator::Span`
//! directly, without the group ever reaching pass 2.5 (this happens whenever
//! the colliding declarations' spans already differ, which is the common
//! case). Such a declaration is genuinely `Span`-keyed but is *absent* from
//! `forced_keys`, so the production `KeyTier::Structural` bucket silently
//! includes it. This census does not use that shortcut: it reads
//! `SealReport::disambiguator_census` (`seal-census`-gated, added alongside
//! this file), which records the exact `Disambiguator` variant that minted
//! every declaration's final id, with no blind spot.
//!
//! # Running
//!
//! ```text
//! cargo test -p nudox-languages --test rust_disambiguator_census \
//!   --features seal-census -- --ignored --nocapture
//! ```
//!
//! `--features seal-census` is required at the `nudox-languages` /
//! `nudox-ir` edge — without it `SealReport::disambiguator_census` and
//! `SealReport::escalation_rounds` do not exist, and this file does not
//! compile (`required-features` in `Cargo.toml` keeps a plain `cargo test`
//! from ever trying). `#[ignore]`d for the same reason as `corpus_sweep.rs`:
//! it drives in-process rust-analyzer over 23 real cargo workspaces.
//!
//! Reuses the corpus table and `produce()` harness from `corpus_sweep.rs`
//! (`tests/rust/common/mod.rs`) rather than inventing a new one.

mod common;

use std::collections::BTreeMap;

use nudox_ir::change::{EcosystemId, PackageLineageId, PackageName};
use nudox_ir::foreign::Unlinked;
use nudox_ir::intro::DisambiguatorKind;
use nudox_ir::kind::KindDiscriminant;
use nudox_ir::package::Escalation;
use nudox_ir::reflect::moniker_path;
use nudox_languages::rust::RustProducer;
use nudox_languages::{PackageSource, ProducerError, produce};

use common::{ENTRIES, chain, entry_root};

// ── Per-package accumulator ─────────────────────────────────────────────────

#[derive(Default, Clone, Copy)]
struct Tally {
    none: usize,
    fn_overload: usize,
    trait_impl: usize,
    span: usize,
    ordinal: usize,
}

impl Tally {
    fn bump(&mut self, k: DisambiguatorKind) {
        match k {
            DisambiguatorKind::None => self.none += 1,
            DisambiguatorKind::FnOverload => self.fn_overload += 1,
            DisambiguatorKind::TraitImpl => self.trait_impl += 1,
            DisambiguatorKind::Span => self.span += 1,
            DisambiguatorKind::Ordinal => self.ordinal += 1,
        }
    }

    fn total(&self) -> usize {
        self.none + self.fn_overload + self.trait_impl + self.span + self.ordinal
    }

    fn f_none(&self) -> usize {
        self.none
    }

    fn f_struct(&self) -> usize {
        self.fn_overload + self.trait_impl
    }

    fn f_unstable(&self) -> usize {
        self.span + self.ordinal
    }

    fn add(&mut self, other: &Tally) {
        self.none += other.none;
        self.fn_overload += other.fn_overload;
        self.trait_impl += other.trait_impl;
        self.span += other.span;
        self.ordinal += other.ordinal;
    }
}

/// One provisioned package's full measurement.
struct PkgCensus {
    dir: &'static str,
    declarations: usize,
    tally: Tally,
    /// `(bucket, KindDiscriminant) -> count`, bucket in {"f_none", "f_struct",
    /// "f_unstable"}.
    kind_tally: BTreeMap<(&'static str, KindDiscriminant), usize>,
    null_path: usize,
    escalation_rounds: usize,
    forced_groups_span: usize,
    forced_groups_ordinal: usize,
    /// Declarations whose true tag is `Span` but whose id is absent from
    /// `forced_keys` — i.e. entries the *production* `KeyProvenance`
    /// (`store/package.rs`) would misclassify as `KeyTier::Structural`.
    hidden_span_in_production: usize,
    /// Of the `Impl`-kind (TraitImpl-tagged) declarations: how many already
    /// have a rendered name (`impl_display_name`, e.g. "impl Deserialize<'de>
    /// for Mutex<T>") that is unique among its siblings at the same ancestor
    /// path — i.e. `moniker_path` alone already identifies them, with no
    /// disambiguator payload needed at all. `seal.rs`'s `Kind::Impl` arm
    /// mints `Disambiguator::TraitImpl` unconditionally, so this is
    /// independent measurement (grouping `produced.table` by
    /// `(Impl, moniker_path)`), not something `SealReport` records.
    impl_unique_name: usize,
    /// The complement: `Impl`-kind declarations that genuinely share a
    /// rendered name with a sibling and therefore need the skeleton payload
    /// (or its rendered text) to be told apart by path.
    impl_colliding_name: usize,
}

fn bucket_of(k: DisambiguatorKind) -> &'static str {
    match k {
        DisambiguatorKind::None => "f_none",
        DisambiguatorKind::FnOverload | DisambiguatorKind::TraitImpl => "f_struct",
        DisambiguatorKind::Span | DisambiguatorKind::Ordinal => "f_unstable",
    }
}

fn kind_label(k: KindDiscriminant) -> String {
    format!("{k:?}")
}

/// `result/` is a symlink into the nix store, which is read-only. Several
/// corpus packages' `build.rs` probes a `cargo:rustc-cfg` by shelling out to
/// `cargo check`, which needs to write a `Cargo.lock` into the crate
/// directory — that write fails with `Permission denied` on the nix-store
/// path even though nothing about the *census* needs to mutate anything.
///
/// `NUDOX_CENSUS_WRITABLE_CORPUS`, if set, names a directory holding a
/// writable copy of `result/` (e.g. `cp -RL result/. "$dir/" && chmod -R u+w
/// "$dir"`); when a package's directory exists under it, that copy is used
/// instead of the read-only checkout. Falls back to `entry_root` (and
/// therefore to the ordinary "corpus not provisioned" failure path)
/// otherwise, so the test's behaviour without the env var set is unchanged.
fn resolve_root(entry: &common::Entry) -> std::path::PathBuf {
    if let Ok(base) = std::env::var("NUDOX_CENSUS_WRITABLE_CORPUS") {
        let staged = std::path::PathBuf::from(base).join(entry.dir);
        if staged.join("Cargo.toml").is_file() {
            return staged;
        }
    }
    entry_root(entry)
}

// ── The census ───────────────────────────────────────────────────────────────

/// Measure the disambiguator-tier distribution over every provisioned
/// crates.io package, using real `nudox_languages::produce` output — no
/// synthetic fixtures, no extrapolation.
#[test]
#[ignore = "drives in-process rust-analyzer over 23 real cargo workspaces (~20 min); \
            requires --features seal-census"]
fn disambiguator_tier_census_over_the_crates_io_corpus() {
    let mut per_package: Vec<PkgCensus> = Vec::new();
    let mut failed: Vec<(&'static str, String)> = Vec::new();

    for entry in ENTRIES {
        let root = resolve_root(entry);
        if !root.join("Cargo.toml").is_file() {
            failed.push((
                entry.dir,
                format!("no checkout at {} — corpus not provisioned", root.display()),
            ));
            continue;
        }

        let src = PackageSource::new(&root, entry.name, entry.version);
        let lineage = PackageLineageId::new(
            EcosystemId::new("cargo"),
            PackageName::new(entry.name.to_owned()),
        );

        eprintln!(
            "=== sealing {} ({} {}) ===",
            entry.dir, entry.name, entry.version
        );
        let started = std::time::Instant::now();
        let produced = match produce(
            &RustProducer { direct_repo: false },
            &src,
            &lineage,
            &Unlinked,
        ) {
            Ok(p) => p,
            Err(err) => {
                let msg = match &err {
                    ProducerError::DependenciesUnresolved { .. } => {
                        format!("dependency resolution failed: {}", chain(&err))
                    }
                    other => chain(other),
                };
                eprintln!("FAIL {}: {msg}", entry.dir);
                failed.push((entry.dir, msg));
                continue;
            }
        };
        eprintln!(
            "  produced {} entries in {:.1}s",
            produced.table.len(),
            started.elapsed().as_secs_f32()
        );

        let declarations = produced.table.len();

        // Forced-key membership, exactly as production `KeyProvenance::from_seal_report`
        // builds it (`store/package.rs:493-501`) — used only to measure the
        // discrepancy against the true per-declaration tag below, never as the
        // primary source of truth.
        let forced_keys: std::collections::HashSet<_> = produced
            .report
            .forced_keys
            .iter()
            .map(|(id, _)| *id)
            .collect();

        let mut tally = Tally::default();
        let mut kind_tally: BTreeMap<(&'static str, KindDiscriminant), usize> = BTreeMap::new();
        let mut hidden_span_in_production = 0usize;

        for &(intro, kind, disamb) in &produced.report.disambiguator_census {
            tally.bump(disamb);
            *kind_tally.entry((bucket_of(disamb), kind)).or_insert(0) += 1;
            if disamb == DisambiguatorKind::Span && !forced_keys.contains(&intro) {
                hidden_span_in_production += 1;
            }
        }

        assert_eq!(
            tally.total(),
            declarations,
            "{}: disambiguator_census ({} rows) must cover every sealed declaration \
             ({declarations}) — a gap means the census instrumentation is not seeing \
             every entry seal() minted",
            entry.dir,
            tally.total(),
        );

        // The production fix for MCP-SURFACE-PLAN §4.14: `SealReport::
        // non_structural_keys` (read unconditionally, not just under
        // `seal-census`) must name exactly the declarations this census
        // independently finds to be `Span`- or `Ordinal`-tagged — no more
        // (that would be a false positive fragility claim) and no fewer
        // (that is the `hidden_span_in_production` defect this file was
        // built to catch). Equivalently: the count of declarations
        // `KeyProvenance` reports `Structural` (declarations minus
        // `non_structural_keys.len()`) must equal the true Structural count
        // (`f_none` + `f_struct`, i.e. `None`/`FnOverload`/`TraitImpl`).
        assert_eq!(
            produced.report.non_structural_keys.len(),
            tally.f_unstable(),
            "{}: SealReport::non_structural_keys ({} entries) must equal the true \
             non-Structural count the census measured ({}) — a mismatch means \
             KeyProvenance is still misreporting some declaration's keyTier",
            entry.dir,
            produced.report.non_structural_keys.len(),
            tally.f_unstable(),
        );
        assert_eq!(
            declarations - produced.report.non_structural_keys.len(),
            tally.f_none() + tally.f_struct(),
            "{}: declarations reported Structural ({}) must equal total minus the \
             true non-Structural count ({})",
            entry.dir,
            declarations - produced.report.non_structural_keys.len(),
            tally.f_none() + tally.f_struct(),
        );

        let null_path = produced
            .table
            .iter()
            .filter(|(intro, _)| moniker_path(&produced.table, *intro).is_none())
            .count();

        let forced_groups_span = produced
            .report
            .forced
            .iter()
            .filter(|f| f.escalated_to == Escalation::Span)
            .count();
        let forced_groups_ordinal = produced
            .report
            .forced
            .iter()
            .filter(|f| f.escalated_to == Escalation::Ordinal)
            .count();

        // Independent check on the `TraitImpl` bucket: group every declaration
        // whose *final* tag is `TraitImpl` by its rendered `moniker_path`
        // (ancestor path + the impl's own display name, e.g. "impl
        // Deserialize<'de> for Mutex<T>") and see whether that name is
        // already unique among siblings. `seal.rs` mints
        // `Disambiguator::TraitImpl` unconditionally for every `Kind::Impl`
        // entry — but a handful can be escalated *past* TraitImpl by pass 2.5
        // (two impls whose full skeleton — trait, self, generics, wheres,
        // negativity, blanket — is byte-identical, so TraitImpl itself
        // collides); those show up as `Impl` under `f_unstable` instead. This
        // block keys off `disambiguator_census`'s own tag, not `Kind::Impl`
        // membership, so it is exactly the complement of that unstable-Impl
        // count and cannot double-count or miss entries.
        let trait_impl_ids: std::collections::HashSet<_> = produced
            .report
            .disambiguator_census
            .iter()
            .filter(|(_, _, tag)| *tag == DisambiguatorKind::TraitImpl)
            .map(|(intro, _, _)| *intro)
            .collect();
        assert_eq!(
            trait_impl_ids.len(),
            tally.trait_impl,
            "{}: disambiguator_census TraitImpl rows ({}) must match the tally ({}) — \
             both are read from the same list, so a mismatch means this file's own \
             counting is wrong",
            entry.dir,
            trait_impl_ids.len(),
            tally.trait_impl,
        );
        let mut impl_by_path: BTreeMap<String, usize> = BTreeMap::new();
        let mut impl_traitimpl_tagged_null_path = 0usize;
        for &intro in &trait_impl_ids {
            match moniker_path(&produced.table, intro) {
                Some(path) => *impl_by_path.entry(path).or_insert(0) += 1,
                None => impl_traitimpl_tagged_null_path += 1,
            }
        }
        let impl_unique_name: usize = impl_by_path.values().filter(|&&c| c == 1).sum();
        let impl_colliding_name: usize = impl_by_path.values().filter(|&&c| c >= 2).sum();
        if impl_unique_name + impl_colliding_name + impl_traitimpl_tagged_null_path
            != tally.trait_impl
        {
            eprintln!(
                "  NOTE {}: impl-by-path accounting ({} unique + {} colliding + {} null-path \
                 = {}) does not sum to the TraitImpl tally ({}) — investigate before trusting \
                 this package's impl_unique/impl_colliding numbers",
                entry.dir,
                impl_unique_name,
                impl_colliding_name,
                impl_traitimpl_tagged_null_path,
                impl_unique_name + impl_colliding_name + impl_traitimpl_tagged_null_path,
                tally.trait_impl,
            );
        }

        eprintln!(
            "  tiers: none={} fn_overload={} trait_impl={} span={} ordinal={} \
             (f_none={} f_struct={} f_unstable={}); null_path={null_path}; \
             escalation_rounds={}; forced_groups(span={forced_groups_span}, \
             ordinal={forced_groups_ordinal}); hidden_span_in_production={hidden_span_in_production}; \
             impl_unique_name={impl_unique_name} impl_colliding_name={impl_colliding_name}",
            tally.none,
            tally.fn_overload,
            tally.trait_impl,
            tally.span,
            tally.ordinal,
            tally.f_none(),
            tally.f_struct(),
            tally.f_unstable(),
            produced.report.escalation_rounds,
        );

        per_package.push(PkgCensus {
            dir: entry.dir,
            declarations,
            tally,
            kind_tally,
            null_path,
            escalation_rounds: produced.report.escalation_rounds,
            forced_groups_span,
            forced_groups_ordinal,
            impl_unique_name,
            impl_colliding_name,
            hidden_span_in_production,
        });
    }

    // ── Aggregate ────────────────────────────────────────────────────────────

    let total_declarations: usize = per_package.iter().map(|p| p.declarations).sum();
    let mut total_tally = Tally::default();
    let mut total_kind_tally: BTreeMap<(&'static str, KindDiscriminant), usize> = BTreeMap::new();
    let mut total_null_path = 0usize;
    let mut max_escalation_rounds = 0usize;
    let mut total_forced_groups_span = 0usize;
    let mut total_forced_groups_ordinal = 0usize;
    let mut total_hidden_span = 0usize;
    let mut total_impl_unique_name = 0usize;
    let mut total_impl_colliding_name = 0usize;

    for p in &per_package {
        total_tally.add(&p.tally);
        for (k, v) in &p.kind_tally {
            *total_kind_tally.entry(*k).or_insert(0) += v;
        }
        total_null_path += p.null_path;
        max_escalation_rounds = max_escalation_rounds.max(p.escalation_rounds);
        total_forced_groups_span += p.forced_groups_span;
        total_forced_groups_ordinal += p.forced_groups_ordinal;
        total_hidden_span += p.hidden_span_in_production;
        total_impl_unique_name += p.impl_unique_name;
        total_impl_colliding_name += p.impl_colliding_name;
    }

    eprintln!(
        "\n=== CENSUS SUMMARY: {} of {} packages sealed ===",
        per_package.len(),
        ENTRIES.len()
    );
    eprintln!("total declarations: {total_declarations}");
    eprintln!(
        "Disambiguator variant distribution:\n  \
         None        {:>7}  ({:.2}%)\n  \
         FnOverload  {:>7}  ({:.2}%)\n  \
         TraitImpl   {:>7}  ({:.2}%)\n  \
         Span        {:>7}  ({:.2}%)\n  \
         Ordinal     {:>7}  ({:.2}%)",
        total_tally.none,
        pct(total_tally.none, total_declarations),
        total_tally.fn_overload,
        pct(total_tally.fn_overload, total_declarations),
        total_tally.trait_impl,
        pct(total_tally.trait_impl, total_declarations),
        total_tally.span,
        pct(total_tally.span, total_declarations),
        total_tally.ordinal,
        pct(total_tally.ordinal, total_declarations),
    );
    eprintln!(
        "Path-addressability buckets:\n  \
         f_none      {:>7}  ({:.2}%)\n  \
         f_struct    {:>7}  ({:.2}%)\n  \
         f_unstable  {:>7}  ({:.2}%)",
        total_tally.f_none(),
        pct(total_tally.f_none(), total_declarations),
        total_tally.f_struct(),
        pct(total_tally.f_struct(), total_declarations),
        total_tally.f_unstable(),
        pct(total_tally.f_unstable(), total_declarations),
    );
    eprintln!(
        "null-path declarations: {total_null_path} ({:.2}%)",
        pct(total_null_path, total_declarations)
    );
    eprintln!(
        "escalation: {total_forced_groups_span} group(s) settled at Span, \
         {total_forced_groups_ordinal} group(s) escalated to Ordinal; \
         max observed pass-2.5 rounds in one seal() call = {max_escalation_rounds} (MAX_ROUNDS=8)"
    );
    eprintln!(
        "production KeyProvenance discrepancy: {total_hidden_span} declaration(s) are truly \
         Disambiguator::Span but ABSENT from SealReport::forced_keys, so \
         nudox-engine's KeyProvenance::from_seal_report / PackageView::key_tier \
         currently reports them as KeyTier::Structural"
    );
    eprintln!(
        "TraitImpl breakdown (f_struct is {} Impl-kind declarations, 0 FnOverload observed): \
         {total_impl_unique_name} ({:.2}% of TraitImpl) already have a unique rendered \
         moniker_path (impl_display_name) among siblings — already path-addressable today \
         with ZERO new seal output, despite Disambiguator != None; {total_impl_colliding_name} \
         ({:.2}% of TraitImpl) genuinely share a rendered name with a sibling and need the \
         skeleton (or its rendered text) to be told apart by path",
        total_tally.trait_impl,
        pct(total_impl_unique_name, total_tally.trait_impl),
        pct(total_impl_colliding_name, total_tally.trait_impl),
    );

    eprintln!("\n--- per-package ---");
    for p in &per_package {
        eprintln!(
            "{:<24} decl={:<7} none={:<6} fn_ovl={:<5} trait_impl={:<6} span={:<6} ordinal={:<6} \
             null_path={:<4} esc_rounds={} hidden_span={} impl_unique={} impl_colliding={}",
            p.dir,
            p.declarations,
            p.tally.none,
            p.tally.fn_overload,
            p.tally.trait_impl,
            p.tally.span,
            p.tally.ordinal,
            p.null_path,
            p.escalation_rounds,
            p.hidden_span_in_production,
            p.impl_unique_name,
            p.impl_colliding_name,
        );
    }

    eprintln!("\n--- f_unstable (Span | Ordinal) by KindDiscriminant, total across corpus ---");
    let mut unstable_by_kind: BTreeMap<KindDiscriminant, usize> = BTreeMap::new();
    for ((bucket, kind), count) in &total_kind_tally {
        if *bucket == "f_unstable" {
            *unstable_by_kind.entry(*kind).or_insert(0) += count;
        }
    }
    let mut unstable_sorted: Vec<_> = unstable_by_kind.into_iter().collect();
    unstable_sorted.sort_by(|a, b| b.1.cmp(&a.1));
    for (kind, count) in &unstable_sorted {
        eprintln!(
            "  {:<12} {:>7}  ({:.2}% of f_unstable, {:.2}% of all declarations)",
            kind_label(*kind),
            count,
            pct(*count, total_tally.f_unstable()),
            pct(*count, total_declarations),
        );
    }

    eprintln!("\n--- f_none by KindDiscriminant, total across corpus ---");
    let mut none_by_kind: BTreeMap<KindDiscriminant, usize> = BTreeMap::new();
    for ((bucket, kind), count) in &total_kind_tally {
        if *bucket == "f_none" {
            *none_by_kind.entry(*kind).or_insert(0) += count;
        }
    }
    let mut none_sorted: Vec<_> = none_by_kind.into_iter().collect();
    none_sorted.sort_by(|a, b| b.1.cmp(&a.1));
    for (kind, count) in &none_sorted {
        eprintln!(
            "  {:<12} {:>7}  ({:.2}% of f_none)",
            kind_label(*kind),
            count,
            pct(*count, total_tally.f_none())
        );
    }

    eprintln!("\n--- f_struct by KindDiscriminant, total across corpus ---");
    let mut struct_by_kind: BTreeMap<KindDiscriminant, usize> = BTreeMap::new();
    for ((bucket, kind), count) in &total_kind_tally {
        if *bucket == "f_struct" {
            *struct_by_kind.entry(*kind).or_insert(0) += count;
        }
    }
    let mut struct_sorted: Vec<_> = struct_by_kind.into_iter().collect();
    struct_sorted.sort_by(|a, b| b.1.cmp(&a.1));
    for (kind, count) in &struct_sorted {
        eprintln!(
            "  {:<12} {:>7}  ({:.2}% of f_struct)",
            kind_label(*kind),
            count,
            pct(*count, total_tally.f_struct())
        );
    }

    if !failed.is_empty() {
        eprintln!("\n--- packages that failed to load ---");
        for (dir, msg) in &failed {
            eprintln!("  FAIL {dir}: {msg}");
        }
    }

    assert!(
        !per_package.is_empty(),
        "not one of the {} crates.io corpus entries sealed — a census that measured \
         nothing must not read as a pass",
        ENTRIES.len(),
    );
}

fn pct(n: usize, of: usize) -> f64 {
    if of == 0 {
        0.0
    } else {
        (n as f64 / of as f64) * 100.0
    }
}
