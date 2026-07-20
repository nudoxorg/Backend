//! On-disk repository size + seal-overhead measurements.
//!
//!   cargo test -p nudox-ir-vcs --release -- --ignored --nocapture repo_size
//!
//! Answers two questions concretely:
//!   1. How big does the pijul store get in the *worst case* (every version
//!      rewrites every symbol — no content sharing) vs the shared case (small
//!      per-version deltas)?
//!   2. How much does a *seal* add on top, and does that grow with history?
//!
//! The structural assertions are machine-independent (ratios, not absolute
//! bytes); the printed table gives real numbers.

#![cfg(test)]

use std::path::Path;

use libpijul::changestore::filesystem::FileSystem as FsChanges;

use nudox_ir::change::{EcosystemId, IntroId, PackageLineageId, PackageName};
use nudox_ir::apply::PristineIntroTable;
use nudox_ir::kind::KindDiscriminant;
use nudox_ir::symbol::Visibility;
use nudox_ir::wire::{EntryPayloadFlags, FunctionWire, KindWire, OwnedEntryPayload, SymbolWire};

use crate::repo::IrRepository;
use crate::version::VersionLabel;

fn pkg() -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("mylib"))
}
fn intro_n(n: u32) -> IntroId {
    let mut b = [0u8; 32];
    b[..4].copy_from_slice(&n.to_le_bytes());
    IntroId::from_raw(b)
}
fn func(name: &str) -> OwnedEntryPayload {
    let sym = SymbolWire {
        name: name.to_owned(),
        visibility: Visibility::Public,
        documentation: None,
        source_path: "src/lib.rs".to_owned(),
        span_start: 0,
        span_end: name.len() as u32,
        aliases: Vec::new(),
        deprecation: None,
        doc_links: Vec::new(),
        attrs: Vec::new(),
        cfg: None,
    };
    OwnedEntryPayload::sealed(
        sym,
        KindDiscriminant::Function,
        KindWire::Function(FunctionWire { input_params: Box::new([]), output_params: Box::new([]), sig: Default::default(), generics: Box::new([]), wheres: Box::new([]) }),
        EntryPayloadFlags::default(),
    )
}
fn vl(s: &str) -> VersionLabel {
    VersionLabel::new(s).unwrap()
}

/// Total bytes at `path`, whether it is a single file (e.g. the sanakirja
/// pristine) or a directory (recursively; e.g. the changestore).
fn path_size(path: &Path) -> u64 {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => meta.len(),
        Ok(meta) if meta.is_dir() => {
            let mut total = 0;
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    total += path_size(&entry.path());
                }
            }
            total
        }
        _ => 0,
    }
}

/// Worst case: every symbol's blob changes every generation (no content sharing).
fn table_worst(n: u32, generation: u32) -> PristineIntroTable {
    let mut t = PristineIntroTable::new();
    for i in 0..n {
        t.insert_live(intro_n(i), func(&format!("s{i}_r{generation}")), None);
    }
    t
}

/// Shared case: generation `g` changes only the `delta` symbols in its rolling
/// window, so successive versions share all but `delta` symbols.
fn table_shared(n: u32, generation: u32, delta: u32) -> PristineIntroTable {
    let mut t = PristineIntroTable::new();
    for i in 0..n {
        let changed_at = if i < generation * delta { i / delta + 1 } else { 0 };
        t.insert_live(intro_n(i), func(&format!("s{i}_r{changed_at}")), None);
    }
    t
}

struct Sizes {
    pristine: u64,
    changes: u64,
}
impl Sizes {
    fn measure(root: &Path) -> Self {
        Self {
            pristine: path_size(&root.join("pristine")),
            changes: path_size(&root.join("changes")),
        }
    }
    fn total(&self) -> u64 {
        self.pristine + self.changes
    }
}

#[test]
#[ignore = "durable size measurement; run with --ignored --nocapture"]
fn repo_size_and_seal_overhead() {
    const N: u32 = 400; // symbols
    const V: u32 = 12; // versions
    const DELTA: u32 = 8; // symbols changed per version in the shared case

    // --- worst case: every version rewrites every symbol ---
    let worst_dir = tempfile::tempdir().unwrap();
    let seal_len = |repo: &IrRepository<FsChanges>, v: &str| {
        repo.seal_from_index(&repo.materialize_version(&vl(v)).unwrap()).unwrap().bytes.len()
    };
    let (worst_seal_first, worst_seal_last) = {
        let repo: IrRepository<FsChanges> =
            IrRepository::open(worst_dir.path(), pkg(), "main").unwrap();
        for generation in 1..=V {
            repo.record_generation(&table_worst(N, generation)).unwrap();
            repo.tag_version(&vl(&format!("{generation}.0.0"))).unwrap();
        }
        // Seal size at the first vs. the last version — same N, different history
        // depth, so this measures history-independence of a seal.
        (seal_len(&repo, "1.0.0"), seal_len(&repo, &format!("{V}.0.0")))
    };
    let worst = Sizes::measure(worst_dir.path());

    // --- shared case: each version changes only DELTA symbols ---
    let shared_dir = tempfile::tempdir().unwrap();
    {
        let repo: IrRepository<FsChanges> =
            IrRepository::open(shared_dir.path(), pkg(), "main").unwrap();
        for generation in 1..=V {
            repo.record_generation(&table_shared(N, generation, DELTA)).unwrap();
            repo.tag_version(&vl(&format!("{generation}.0.0"))).unwrap();
        }
    }
    let shared = Sizes::measure(shared_dir.path());

    let kib = |b: u64| b as f64 / 1024.0;
    let kib_u = |b: usize| b as f64 / 1024.0;
    eprintln!("\n=== repo size + seal overhead (N={N} symbols, V={V} versions, Δ={DELTA}) ===");
    eprintln!("                             pristine     changes       total");
    eprintln!(
        "worst case (rewrite all):  {:8.0} KiB {:8.0} KiB {:8.0} KiB",
        kib(worst.pristine), kib(worst.changes), kib(worst.total())
    );
    eprintln!(
        "shared case (Δ per ver):   {:8.0} KiB {:8.0} KiB {:8.0} KiB",
        kib(shared.pristine), kib(shared.changes), kib(shared.total())
    );
    eprintln!("--- seal overhead ---");
    eprintln!("one seal @ v1  (worst):    {:8.1} KiB", kib_u(worst_seal_first));
    eprintln!("one seal @ v{V} (worst):    {:8.1} KiB", kib_u(worst_seal_last));
    eprintln!(
        "K checkpoints add:         K × ~{:.0} KiB  (e.g. 4 hot versions ≈ {:.0} KiB)",
        kib_u(worst_seal_last), kib_u(worst_seal_last * 4)
    );
    eprintln!(
        "seal : worst-repo ratio:   {:.1}%  (one seal vs the whole {V}-version store)",
        100.0 * worst_seal_last as f64 / worst.total() as f64
    );
    eprintln!("========================================================================\n");

    // --- structural assertions (machine-independent) ---

    // 1. No sharing costs a lot: the worst-case changestore is materially larger
    //    than the shared one (each worst version records an N-symbol diff; each
    //    shared version records only a Δ-symbol diff).
    assert!(
        worst.changes > shared.changes * 2,
        "worst-case changes ({}) should dwarf shared ({})",
        worst.changes, shared.changes
    );

    // 2. A seal captures ONE version's IR, not the history: its size is
    //    ~independent of how many versions precede it.
    let ratio = worst_seal_last.max(worst_seal_first) as f64
        / worst_seal_first.min(worst_seal_last).max(1) as f64;
    assert!(
        ratio < 1.3,
        "seal size must be history-independent (v1 {worst_seal_first} vs v{V} {worst_seal_last})"
    );

    // 3. A single seal is smaller than the whole worst-case store (the store
    //    holds all V versions' changes; the seal holds one version).
    assert!(
        (worst_seal_last as u64) < worst.total(),
        "one seal ({worst_seal_last} B) < whole {V}-version repo ({} B)",
        worst.total()
    );
}
