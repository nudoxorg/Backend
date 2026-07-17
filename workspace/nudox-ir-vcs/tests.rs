//! Gate tests for the libpijul-backed IR VCS.
//!
//! These prove the whole pivot works end-to-end: a `PristineIntroTable` records
//! into libpijul, materializes back identically, accumulates history, unrecords,
//! and seals a deterministic archive — with libpijul owning changes/deps/unrecord.

use nudox_change::{
    ChangeId, EcosystemId, IntroId, PackageLineageId, PackageName, StableRef,
};
use nudox_ir::apply::{LinkRecord, PristineIntroTable};
use nudox_ir::kind::KindDiscriminant;
use nudox_ir::wire::{
    EntryPayloadFlags, FunctionWire, KindWire, ModuleWire, OwnedEntryPayload, SymbolWire,
};

use crate::repo::IrRepository;
use crate::serve_cache::{ServeCache, ServeSource};
use crate::version::VersionLabel;

fn pkg() -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("mylib"))
}

fn intro(n: u8) -> IntroId {
    IntroId::from_raw([n; 32])
}

fn sref(i: IntroId) -> StableRef {
    StableRef::new(pkg(), i)
}

fn sym(name: &str) -> SymbolWire {
    SymbolWire {
        name: name.to_owned(),
        visibility: 0,
        documentation: None,
        source_path: "src/lib.rs".to_owned(),
        span_start: 0,
        span_end: name.len() as u32,
        aliases: Vec::new(),
        deprecation: None,
        doc_links: Vec::new(),
    }
}

fn function(name: &str) -> OwnedEntryPayload {
    OwnedEntryPayload::sealed(
        sym(name),
        KindDiscriminant::Function,
        KindWire::Function(FunctionWire { input_params: Box::new([]), output_params: Box::new([]) }),
        EntryPayloadFlags::default(),
    )
}

fn module(name: &str) -> OwnedEntryPayload {
    OwnedEntryPayload::sealed(
        sym(name),
        KindDiscriminant::Module,
        KindWire::Module(ModuleWire {}),
        EntryPayloadFlags::default(),
    )
}

/// A module `root` (intro 1) with a nested function `do_thing` (intro 2) linked
/// to it. Links carry a zero `added_by` because libpijul now owns provenance.
fn sample_ir() -> PristineIntroTable {
    let m = intro(1);
    let f = intro(2);
    let mut ir = PristineIntroTable::new();
    ir.insert_live(m, module("root"), None);
    ir.insert_live(f, function("do_thing"), Some(m));
    ir.insert_link(LinkRecord {
        a: sref(m),
        b: sref(f),
        kind_a: KindDiscriminant::Module,
        kind_b: KindDiscriminant::Function,
        added_by: ChangeId::from_raw([0u8; 32]),
    });
    ir
}

#[test]
fn record_then_materialize_round_trips() {
    let repo = IrRepository::in_memory(pkg(), "main").unwrap();
    let ir = sample_ir();

    let hash = repo.record_generation(&ir).unwrap();
    assert!(hash.is_some(), "recording a non-empty IR must produce a change");

    let mat = repo.materialize().unwrap();
    assert_eq!(mat.live_entries().count(), 2);
    assert!(mat.is_live(intro(1)) && mat.is_live(intro(2)));
    assert_eq!(mat.parent_of(intro(2)), Some(intro(1)), "nesting must survive the round-trip");
    assert_eq!(
        mat.get(intro(2)).and_then(|e| e.as_live()),
        Some(&function("do_thing")),
        "payload bytes must survive the round-trip",
    );
    assert_eq!(mat.links().count(), 1, "the link must survive the round-trip");
    let link = mat.links().next().unwrap();
    assert_eq!(link.a, sref(intro(1)));
    assert_eq!(link.b, sref(intro(2)));
}

#[test]
fn re_recording_identical_ir_is_a_noop() {
    let repo = IrRepository::in_memory(pkg(), "main").unwrap();
    let ir = sample_ir();
    assert!(repo.record_generation(&ir).unwrap().is_some());
    assert!(
        repo.record_generation(&ir).unwrap().is_none(),
        "re-recording the same IR must record no change",
    );
}

#[test]
fn history_accumulates_across_generations() {
    let repo = IrRepository::in_memory(pkg(), "main").unwrap();
    let m = intro(1);

    // Gen A: just the module.
    let mut a = PristineIntroTable::new();
    a.insert_live(m, module("root"), None);
    let ha = repo.record_generation(&a).unwrap().unwrap();

    // Gen B: module + a function.
    let mut b = a.clone();
    b.insert_live(intro(2), function("do_thing"), Some(m));
    let hb = repo.record_generation(&b).unwrap().unwrap();

    let log = repo.log().unwrap();
    assert_eq!(log.len(), 2, "two generations → two changes");
    assert!(repo.has_change(&ha).unwrap());
    assert!(repo.has_change(&hb).unwrap());
    assert_ne!(ha, hb, "distinct generations have distinct change hashes");

    let mat = repo.materialize().unwrap();
    assert_eq!(mat.live_entries().count(), 2, "tip reflects gen B");
}

#[test]
fn unrecord_reverts_to_prior_generation() {
    let repo = IrRepository::in_memory(pkg(), "main").unwrap();
    let m = intro(1);

    let mut a = PristineIntroTable::new();
    a.insert_live(m, module("root"), None);
    repo.record_generation(&a).unwrap().unwrap();

    let mut b = a.clone();
    b.insert_live(intro(2), function("do_thing"), Some(m));
    let hb = repo.record_generation(&b).unwrap().unwrap();
    assert_eq!(repo.materialize().unwrap().live_entries().count(), 2);

    // Undo gen B — libpijul owns the unrecord.
    repo.unrecord(&hb).unwrap();
    assert_eq!(repo.log().unwrap().len(), 1, "unrecord drops the change from history");

    let mat = repo.materialize().unwrap();
    assert_eq!(mat.live_entries().count(), 1, "tip reverts to gen A");
    assert!(mat.is_live(m));
    assert!(!mat.is_live(intro(2)));
}

#[test]
fn seal_is_deterministic() {
    let repo = IrRepository::in_memory(pkg(), "main").unwrap();
    repo.record_generation(&sample_ir()).unwrap().unwrap();

    let sealed1 = repo.seal().unwrap();
    assert!(!sealed1.bytes.is_empty(), "a sealed archive has bytes");
    let sealed2 = repo.seal().unwrap();
    assert_eq!(
        sealed1.cas_key, sealed2.cas_key,
        "sealing the same materialized state twice must yield the same CasKey",
    );
}

#[test]
fn durable_open_persists_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // Record a generation through the on-disk (fs changestore + sanakirja) store,
    // then drop the repository entirely.
    let hash = {
        let repo = IrRepository::open(root, pkg(), "main").unwrap();
        repo.record_generation(&sample_ir()).unwrap().expect("a change was recorded")
    };

    // Re-open from the same root: the pristine and the change both persisted.
    let repo2 = IrRepository::open(root, pkg(), "main").unwrap();
    assert!(repo2.has_change(&hash).unwrap(), "the recorded change survives a re-open");
    assert_eq!(repo2.log().unwrap().len(), 1);
    let mat = repo2.materialize().unwrap();
    assert_eq!(mat.live_entries().count(), 2, "materialized IR survives a re-open");
    assert!(mat.is_live(intro(1)) && mat.is_live(intro(2)));
}

#[test]
fn seal_archive_serves_zero_copy() {
    // The full serve path: record -> seal (borrowed, no owned table) -> open the
    // zerocopy archive -> answer lookups from POD indices and read a payload body
    // back as a borrowed SymbolView (IO-speed, zero decode).
    let repo = IrRepository::in_memory(pkg(), "main").unwrap();
    repo.record_generation(&sample_ir()).unwrap().unwrap();

    let sealed = repo.seal().unwrap();
    let yoked = sealed.open().expect("archive opens");
    let view = yoked.get();

    // Hot-path lookups straight from the zerocopy indices.
    let m = view.lookup_intro(intro(1)).expect("module in archive");
    let f = view.lookup_intro(intro(2)).expect("function in archive");
    assert!(view.lookup_name("root").collect::<Vec<_>>().contains(&m));
    assert!(view.lookup_name("do_thing").collect::<Vec<_>>().contains(&f));
    assert!(view.children(m).collect::<Vec<_>>().contains(&f), "function is a child of the module");

    // Payload body = the textual blob, read back as a borrowed SymbolView.
    let raw = view.payload_raw(f).expect("payload bytes");
    let sv = crate::blob::SymbolView::from_bytes(raw).expect("blob parses");
    assert_eq!(sv.name(), "do_thing");
    assert_eq!(sv.parent(), Some(intro(1)));

    // Sealing again yields the same content address.
    assert_eq!(repo.seal().unwrap().cas_key, sealed.cas_key);
}

// ===========================================================================
// Historical replay
// ===========================================================================

fn vlabel(s: &str) -> VersionLabel {
    VersionLabel::new(s).unwrap()
}

/// A flat table of top-level functions (no parents, no links) keyed by intro.
fn func_table(entries: &[(u8, &str)]) -> PristineIntroTable {
    let mut t = PristineIntroTable::new();
    for (n, name) in entries {
        t.insert_live(intro(*n), function(name), None);
    }
    t
}

/// **The load-bearing property: a tagged version is frozen.**
///
/// Tag `v1` at generation A, then keep recording onto `main`. Reconstructing
/// `v1` must still yield generation A's IR — the fork shares the graph but its
/// tip never moves.
#[test]
fn tagged_version_is_frozen_against_later_records() {
    let repo = IrRepository::in_memory(pkg(), "main").unwrap();

    repo.record_generation(&func_table(&[(1, "a"), (2, "b")])).unwrap().unwrap();
    let v1_state = repo.tag_version(&vlabel("1.0.0")).unwrap();

    // main advances: change `b`, add `c`.
    repo.record_generation(&func_table(&[(1, "a"), (2, "b_v2"), (3, "c")])).unwrap().unwrap();
    let v2_state = repo.tag_version(&vlabel("2.0.0")).unwrap();

    assert_ne!(v1_state.to_bytes(), v2_state.to_bytes(), "distinct versions, distinct states");

    // v1 reconstructs generation A exactly.
    let v1 = repo.materialize_version(&vlabel("1.0.0")).unwrap();
    assert_eq!(v1.len(), 2, "v1 has exactly the two original symbols");
    assert_eq!(payload_name(&v1, intro(2)), "b", "v1's `b` is the original, not b_v2");
    assert!(v1.get(intro(3)).is_none(), "v1 predates `c`");

    // v2 reconstructs generation B.
    let v2 = repo.materialize_version(&vlabel("2.0.0")).unwrap();
    assert_eq!(v2.len(), 3);
    assert_eq!(payload_name(&v2, intro(2)), "b_v2");
    assert!(v2.get(intro(3)).is_some(), "v2 has `c`");

    // version_state is a cheap read matching the tag-time state.
    assert_eq!(repo.version_state(&vlabel("1.0.0")).unwrap().to_bytes(), v1_state.to_bytes());
}

fn payload_name(idx: &crate::checkout::MaterializedIndex, i: IntroId) -> String {
    let view = idx.view(i).unwrap().unwrap();
    view.name().to_owned()
}

/// Tagging is strict: a double-tag errors, and serving an untagged version errors.
#[test]
fn tagging_is_strict() {
    let repo = IrRepository::in_memory(pkg(), "main").unwrap();
    repo.record_generation(&func_table(&[(1, "a")])).unwrap().unwrap();

    repo.tag_version(&vlabel("1.0.0")).unwrap();
    assert!(
        matches!(repo.tag_version(&vlabel("1.0.0")), Err(crate::VcsError::VersionAlreadyTagged { .. })),
        "re-tagging the same version must error"
    );
    assert!(
        matches!(repo.materialize_version(&vlabel("9.9.9")), Err(crate::VcsError::VersionNotFound { .. })),
        "serving an untagged version must error"
    );
    assert!(
        matches!(repo.version_state(&vlabel("9.9.9")), Err(crate::VcsError::VersionNotFound { .. })),
    );
}

/// A single symbol can be checked out at a specific version — O(symbol) partial
/// replay — and reflects that version's bytes, not `main`'s.
#[test]
fn checkout_symbol_at_version() {
    let repo = IrRepository::in_memory(pkg(), "main").unwrap();
    repo.record_generation(&func_table(&[(1, "a"), (2, "b")])).unwrap().unwrap();
    repo.tag_version(&vlabel("1.0.0")).unwrap();
    repo.record_generation(&func_table(&[(1, "a"), (2, "b_v2")])).unwrap().unwrap();

    // At v1, symbol 2 is `b`.
    let raw = repo.checkout_symbol_at(&vlabel("1.0.0"), intro(2)).unwrap().expect("present at v1");
    let view = crate::blob::SymbolView::from_bytes(&raw).unwrap();
    assert_eq!(view.name(), "b", "v1 checkout is the original payload");

    // On main it's `b_v2`.
    let raw_main = repo.checkout_symbol(intro(2)).unwrap().unwrap();
    assert_eq!(crate::blob::SymbolView::from_bytes(&raw_main).unwrap().name(), "b_v2");

    // A symbol absent from the version is `None`.
    assert!(repo.checkout_symbol_at(&vlabel("1.0.0"), intro(99)).unwrap().is_none());
}

/// Per-symbol history: every change touching a symbol, native via `log_for_path`.
/// Only-write-changed keeps unrelated symbols out of a symbol's history.
#[test]
fn symbol_history_tracks_a_symbol_across_generations() {
    let repo = IrRepository::in_memory(pkg(), "main").unwrap();

    // Gen A: create f1 + f2.
    repo.record_generation(&func_table(&[(1, "f1"), (2, "f2")])).unwrap().unwrap();
    // Gen B: change f1 only.
    repo.record_generation(&func_table(&[(1, "f1_v2"), (2, "f2")])).unwrap().unwrap();
    // Gen C: change f2 only.
    repo.record_generation(&func_table(&[(1, "f1_v2"), (2, "f2_v2")])).unwrap().unwrap();

    let h1 = repo.symbol_history(intro(1)).unwrap();
    let h2 = repo.symbol_history(intro(2)).unwrap();

    // f1: created in A, changed in B → 2 changes. Gen C (f2 only) must NOT appear.
    assert_eq!(h1.len(), 2, "f1 touched by exactly gens A and B, got {h1:?}");
    // f2: created in A, changed in C → 2 changes. Gen B (f1 only) must NOT appear.
    assert_eq!(h2.len(), 2, "f2 touched by exactly gens A and C, got {h2:?}");

    // The two symbols' histories are genuinely different (independent files).
    assert_ne!(h1, h2, "distinct symbols have distinct histories");

    // A symbol absent from the tip has empty history.
    assert!(repo.symbol_history(intro(200)).unwrap().is_empty());
}

/// `diff_versions` reports adds/removes/modifies at symbol granularity;
/// `changes_between` gives the native pijul change delta.
#[test]
fn diff_versions_and_changes_between() {
    let repo = IrRepository::in_memory(pkg(), "main").unwrap();

    repo.record_generation(&func_table(&[(1, "a"), (2, "b"), (3, "c")])).unwrap().unwrap();
    repo.tag_version(&vlabel("1.0.0")).unwrap();

    // v2: modify `b`, remove `c`, add `d`.
    repo.record_generation(&func_table(&[(1, "a"), (2, "b_v2"), (4, "d")])).unwrap().unwrap();
    repo.tag_version(&vlabel("2.0.0")).unwrap();

    let diff = repo.diff_versions(&vlabel("1.0.0"), &vlabel("2.0.0")).unwrap();
    assert_eq!(diff.added, vec![intro(4)], "added d");
    assert_eq!(diff.removed, vec![intro(3)], "removed c");
    assert_eq!(diff.modified, vec![intro(2)], "modified b");
    assert!(!diff.is_empty());

    // Identical versions diff to empty.
    let same = repo.diff_versions(&vlabel("1.0.0"), &vlabel("1.0.0")).unwrap();
    assert!(same.is_empty());

    // changes_between: the single change recorded after v1 was tagged.
    let delta = repo.changes_between(&vlabel("1.0.0"), &vlabel("2.0.0")).unwrap();
    assert_eq!(delta.len(), 1, "one generation separates v1 and v2");
    // And it is exactly the second change in main's log.
    let full_log = repo.log().unwrap();
    assert_eq!(delta[0], full_log[1]);
}

/// Tagging N versions creates NO new changes — a version is a graph-sharing
/// fork, not a snapshot. This is the storage argument made mechanical.
#[test]
fn tagging_does_not_duplicate_content() {
    let repo = IrRepository::in_memory(pkg(), "main").unwrap();
    repo.record_generation(&func_table(&[(1, "a"), (2, "b")])).unwrap().unwrap();
    let changes_before = repo.log().unwrap().len();

    for v in ["1.0.0", "1.0.1", "1.0.2", "1.0.3", "1.0.4"] {
        repo.tag_version(&vlabel(v)).unwrap();
    }

    // Five version channels, zero new changes: the graph is shared, not copied.
    assert_eq!(
        repo.log().unwrap().len(),
        changes_before,
        "forking versions must add no changes to main's history"
    );
    // Every version still reconstructs the same IR.
    for v in ["1.0.0", "1.0.4"] {
        assert_eq!(repo.materialize_version(&vlabel(v)).unwrap().len(), 2);
    }
}

/// The serve cache: a hit skips the graph walk; the leaky bucket throttles a
/// cold-version burst so it can't evict the hot set.
#[test]
fn serve_cache_hits_and_throttles() {
    use std::num::NonZeroU32;

    let repo = IrRepository::in_memory(pkg(), "main").unwrap();
    repo.record_generation(&func_table(&[(1, "a"), (2, "b")])).unwrap().unwrap();
    repo.tag_version(&vlabel("1.0.0")).unwrap();
    repo.record_generation(&func_table(&[(1, "a"), (2, "b_v2"), (3, "c")])).unwrap().unwrap();
    repo.tag_version(&vlabel("2.0.0")).unwrap();

    // Burst of 1/sec: the first admission stores, the second is throttled.
    let cache = ServeCache::new(8 << 20, NonZeroU32::new(1).unwrap(), NonZeroU32::new(1).unwrap());

    // First serve of v1: miss, admitted, stored.
    let s1 = repo.serve_version_cached(&vlabel("1.0.0"), &cache).unwrap();
    assert_eq!(s1.source, ServeSource::SealedAndStored);

    // Second serve of v1: cache hit (no graph walk), same content address.
    let s1b = repo.serve_version_cached(&vlabel("1.0.0"), &cache).unwrap();
    assert_eq!(s1b.source, ServeSource::Cached);
    assert_eq!(s1b.archive.cas_key, s1.archive.cas_key);

    // A different cold version now: bucket is drained → throttled (served but not
    // stored), so the hot v1 entry is preserved.
    let s2 = repo.serve_version_cached(&vlabel("2.0.0"), &cache).unwrap();
    assert_eq!(s2.source, ServeSource::SealedThrottled);
    assert!(s2.archive.cas_key != s1.archive.cas_key, "v2 is a different archive");

    cache.run_pending_tasks();
    assert!(cache.get(repo.version_state(&vlabel("1.0.0")).unwrap()).is_some(), "hot v1 retained");
    assert!(cache.get(repo.version_state(&vlabel("2.0.0")).unwrap()).is_none(), "cold v2 not stored");

    // The served archive is a real zerocopy archive.
    let yoked = s1.archive.open().unwrap();
    assert!(yoked.get().lookup_intro(intro(1)).is_some());
}

/// **Durable version channels survive a process restart.** Tag a version through
/// the on-disk (sanakirja + fs changestore) repository, drop it, reopen the same
/// root, and reconstruct the frozen version — the fork persists in the pristine.
#[test]
fn tagged_version_persists_across_reopen() {
    use libpijul::changestore::filesystem::FileSystem as FsChanges;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    let v1_state = {
        let repo: IrRepository<FsChanges> = IrRepository::open(root, pkg(), "main").unwrap();
        repo.record_generation(&func_table(&[(1, "a"), (2, "b")])).unwrap().unwrap();
        let s = repo.tag_version(&vlabel("1.0.0")).unwrap();
        // main advances after the tag.
        repo.record_generation(&func_table(&[(1, "a"), (2, "b_v2"), (3, "c")])).unwrap().unwrap();
        s
    };

    // Reopen: the version channel and its frozen state persisted.
    let repo2: IrRepository<FsChanges> = IrRepository::open(root, pkg(), "main").unwrap();
    assert_eq!(
        repo2.version_state(&vlabel("1.0.0")).unwrap().to_bytes(),
        v1_state.to_bytes(),
        "version state survives reopen",
    );

    let v1 = repo2.materialize_version(&vlabel("1.0.0")).unwrap();
    assert_eq!(v1.len(), 2, "frozen v1 still has exactly two symbols after reopen");
    assert_eq!(payload_name(&v1, intro(2)), "b", "frozen v1's `b` is the original");

    // Partial checkout at the persisted version.
    let raw = repo2.checkout_symbol_at(&vlabel("1.0.0"), intro(1)).unwrap().unwrap();
    assert_eq!(crate::blob::SymbolView::from_bytes(&raw).unwrap().name(), "a");

    // Per-symbol history survives too (symbol 2 changed once after creation).
    assert_eq!(repo2.symbol_history(intro(2)).unwrap().len(), 2);
}

// ===========================================================================
// Benchmark: replay-vs-snapshot on a synthetic large multi-version package.
//
//   cargo test -p nudox-ir-vcs --release -- --ignored --nocapture bench_replay
//
// Prints a timing table. Not a pass/fail assertion of wall-clock (that would be
// machine-dependent + flaky); it asserts only the structural invariants —
// age-independence of replay, O(delta) writes, and O(symbol) partial reads.
// ===========================================================================

/// A distinct 32-byte intro from a u32 (the u8 `intro()` helper tops out at 256).
fn intro_n(n: u32) -> IntroId {
    let mut b = [0u8; 32];
    b[..4].copy_from_slice(&n.to_le_bytes());
    IntroId::from_raw(b)
}

#[test]
#[ignore = "benchmark; run explicitly with --ignored --nocapture"]
fn bench_replay_vs_snapshot() {
    use std::time::Instant;

    const N: u32 = 1500; // symbols per version
    const VERSIONS: u32 = 6; // number of published versions
    const DELTA: u32 = 8; // symbols changed between versions

    fn table_gen(generation: u32) -> PristineIntroTable {
        // Every symbol's name embeds the generation only if it's in the changed
        // window, so successive generations differ by DELTA symbols.
        let mut t = PristineIntroTable::new();
        for i in 0..N {
            let changed_at = if i < generation * DELTA {
                (i / DELTA + 1).min(generation)
            } else {
                0
            };
            let name = format!("sym_{i}_r{changed_at}");
            t.insert_live(intro_n(i), function(&name), None);
        }
        t
    }

    let repo = IrRepository::in_memory(pkg(), "main").unwrap();

    // --- build + tag VERSIONS generations, measuring write cost ---
    let mut record_times = Vec::new();
    let mut sync_counts = Vec::new();
    for generation in 1..=VERSIONS {
        let t0 = Instant::now();
        repo.record_generation(&table_gen(generation)).unwrap();
        record_times.push(t0.elapsed());
        sync_counts.push(repo.sync_output_count());
        repo.tag_version(&vlabel(&format!("{generation}.0.0"))).unwrap();
    }

    // O(delta) write invariant: after the first (baseline) record, no later
    // record triggers a whole-tree resync.
    let baseline_sync = sync_counts[0];
    assert!(
        sync_counts.iter().all(|c| *c == baseline_sync),
        "steady-state records must not resync: {sync_counts:?}"
    );

    let oldest = vlabel("1.0.0");
    let newest = vlabel(&format!("{VERSIONS}.0.0"));

    // --- full replay: oldest vs newest version (age-independence) ---
    let bench = |label: &VersionLabel| {
        let mut best = std::time::Duration::MAX;
        let mut n = 0;
        for _ in 0..3 {
            let t0 = Instant::now();
            let idx = repo.materialize_version(label).unwrap();
            best = best.min(t0.elapsed());
            n = idx.len();
        }
        (best, n)
    };
    let (replay_old, n_old) = bench(&oldest);
    let (replay_new, n_new) = bench(&newest);
    assert_eq!(n_old, N as usize);
    assert_eq!(n_new, N as usize);

    // --- snapshot: seal newest once, then read it all back from the archive ---
    let idx_new = repo.materialize_version(&newest).unwrap();
    let t0 = Instant::now();
    let sealed = repo.seal_from_index(&idx_new).unwrap();
    let seal_time = t0.elapsed();

    let yoked = sealed.open().unwrap();
    let view = yoked.get();
    let t0 = Instant::now();
    let mut total_payload = 0usize;
    for i in 0..N {
        if let Some(id) = view.lookup_intro(intro_n(i)) {
            let raw = view.payload_raw(id).unwrap();
            let sv = crate::blob::SymbolView::from_bytes(raw).unwrap();
            total_payload += sv.name().len();
        }
    }
    let snapshot_full_read = t0.elapsed();

    // --- partial: one symbol at the newest version (O(symbol)) ---
    let mut checkout_best = std::time::Duration::MAX;
    for _ in 0..5 {
        let t0 = Instant::now();
        let one = repo.checkout_symbol_at(&newest, intro_n(N / 2)).unwrap();
        checkout_best = checkout_best.min(t0.elapsed());
        assert!(one.is_some());
    }

    let ms = |d: std::time::Duration| d.as_secs_f64() * 1000.0;
    eprintln!("\n=== IR historical-replay benchmark (N={N} symbols, {VERSIONS} versions, Δ={DELTA}) ===");
    eprintln!("archive size:                    {} KiB", sealed.bytes.len() / 1024);
    eprintln!("payload names scanned:           {total_payload} bytes");
    eprintln!("--- writes (O(delta) via stat cache) ---");
    eprintln!("baseline record (gen 1):         {:8.2} ms", ms(record_times[0]));
    eprintln!(
        "steady record (gen {VERSIONS}, Δ={DELTA}):      {:8.2} ms   (whole-tree resyncs: {baseline_sync})",
        ms(*record_times.last().unwrap())
    );
    eprintln!("--- full replay (graph walk, O(state), age-independent) ---");
    eprintln!("materialize OLDEST (v1):         {:8.2} ms", ms(replay_old));
    eprintln!("materialize NEWEST (v{VERSIONS}):         {:8.2} ms", ms(replay_new));
    eprintln!("--- snapshot (sealed zerocopy archive) ---");
    eprintln!("seal newest from index:          {:8.2} ms", ms(seal_time));
    eprintln!("full read from mmap archive:     {:8.2} ms", ms(snapshot_full_read));
    eprintln!("--- partial replay ---");
    eprintln!("checkout ONE symbol at v{VERSIONS}:       {:8.3} ms", ms(checkout_best));
    eprintln!(
        "  → one symbol is {:.0}x cheaper than a full replay",
        replay_new.as_secs_f64() / checkout_best.as_secs_f64().max(f64::EPSILON)
    );
    eprintln!("=========================================================\n");

    // Structural invariants (machine-independent):
    // 1. Age-independence: oldest and newest full replays are within 3x.
    let ratio = ms(replay_old).max(ms(replay_new)) / ms(replay_old).min(ms(replay_new)).max(f64::EPSILON);
    assert!(ratio < 3.0, "replay cost must be age-independent (ratio {ratio:.2})");
    // 2. Partial read is strictly cheaper than a full replay.
    assert!(checkout_best < replay_new, "one-symbol checkout must beat a full replay");
}
