use super::*;
use crate::wire::{EntryPayloadFlags, FunctionWire, KindWire, PayloadTable, SymbolWire};
use ir::change::{EcosystemId, PackageName};
use ir::entry::Visibility;
use ir::kind::KindDiscriminant;

fn pkg() -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("mylib"))
}

fn intro(n: u8) -> IntroId {
    IntroId::from_raw([n; 32])
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
        KindWire::Function(FunctionWire {
            input_params: Box::new([]),
            output_params: Box::new([]),
            sig: Default::default(),
            generics: Box::new([]),
            wheres: Box::new([]),
        }),
        EntryPayloadFlags::default(),
    )
}

fn table(entries: &[(u8, &str)]) -> PayloadTable {
    let mut t = PayloadTable::new();
    for (n, name) in entries {
        t.insert_live(intro(*n), func(name), None);
    }
    t
}

/// Changing ONE symbol in a 4-symbol package must re-output exactly ONE file
/// (true O(delta)), reuse the exact `Arc` for the other three, and match a
/// fresh full materialize.
#[test]
fn incremental_output_is_o_delta() {
    let repo = IrRepository::in_memory(pkg(), "main").unwrap();

    // Gen A: 4 symbols.
    repo.record_generation(&table(&[(1, "a"), (2, "b"), (3, "c"), (4, "d")]))
        .unwrap()
        .expect("gen A recorded");
    let prev = repo.materialize_index().unwrap();
    assert_eq!(prev.len(), 4);

    // Gen B: change exactly one symbol (intro 2's payload).
    repo.record_generation(&table(&[(1, "a"), (2, "b_CHANGED"), (3, "c"), (4, "d")]))
        .unwrap()
        .expect("gen B recorded");

    let (idx, count) = repo.materialize_index_incremental_counted(&prev).unwrap();

    // O(delta): the delta path ran and output EXACTLY ONE file (not 4).
    assert_eq!(count, Some(1), "must re-output only the one changed symbol");

    // Result equals a fresh full materialize.
    let full = repo.materialize_index().unwrap();
    assert_eq!(idx.symbols.len(), full.symbols.len());
    for (k, v) in &full.symbols {
        assert_eq!(
            &idx.get(*k).unwrap()[..],
            &v[..],
            "symbol {} bytes must match full",
            k.to_hex()
        );
    }

    // Untouched symbols keep the EXACT same Arc (never re-output); the
    // changed one gets a fresh Arc.
    for n in [1u8, 3, 4] {
        assert!(
            std::sync::Arc::ptr_eq(prev.get(intro(n)).unwrap(), idx.get(intro(n)).unwrap()),
            "untouched symbol {n} must keep its Arc"
        );
    }
    assert!(
        !std::sync::Arc::ptr_eq(prev.get(intro(2)).unwrap(), idx.get(intro(2)).unwrap()),
        "changed symbol must have a fresh Arc"
    );
}

/// Deletion + addition in one step: `checkout`-free incremental drops the
/// removed symbol and adds the new one, output count = added ∪ modified.
#[test]
fn incremental_add_and_remove() {
    let repo = IrRepository::in_memory(pkg(), "main").unwrap();
    repo.record_generation(&table(&[(1, "a"), (2, "b"), (3, "c")]))
        .unwrap()
        .unwrap();
    let prev = repo.materialize_index().unwrap();

    // Remove symbol 2, add symbol 4.
    repo.record_generation(&table(&[(1, "a"), (3, "c"), (4, "d")]))
        .unwrap()
        .unwrap();
    let (idx, count) = repo.materialize_index_incremental_counted(&prev).unwrap();

    assert_eq!(
        count,
        Some(1),
        "only the added symbol needs output; removal needs none"
    );
    let mut intros: Vec<u8> = idx.intros().map(|i| i.as_bytes()[0]).collect();
    intros.sort_unstable();
    assert_eq!(intros, vec![1, 3, 4]);
    // 1 and 3 untouched → same Arc.
    assert!(std::sync::Arc::ptr_eq(
        prev.get(intro(1)).unwrap(),
        idx.get(intro(1)).unwrap()
    ));
    assert!(std::sync::Arc::ptr_eq(
        prev.get(intro(3)).unwrap(),
        idx.get(intro(3)).unwrap()
    ));
}

/// **Regression: a backwards wall-clock step must not drop a modification.**
///
/// libpijul's record stat-cache re-diffs a file only when its mtime is >=
/// the channel's last-modified time. `SystemTime` is not monotonic, so if
/// the clock steps backwards across a second boundary between two
/// recordings, a genuinely modified symbol file would look stale and its
/// change would be silently dropped (observed once as a flake in
/// `checkout::tests::incremental_reuse_untouched_arc`). The write path now
/// clamps every written file's mtime to the channel tip; this test forces
/// the pathological clock (`mock_now = UNIX_EPOCH`) and asserts the
/// modification is still recorded.
#[test]
fn record_generation_survives_backwards_clock_step() {
    let repo = IrRepository::in_memory(pkg(), "main").unwrap();

    repo.record_generation(&table(&[(1, "root"), (2, "helper")]))
        .unwrap()
        .expect("gen A recorded");

    // Simulate the wall clock stepping (far) backwards before gen B.
    repo.mock_now.set(Some(std::time::UNIX_EPOCH));

    let recorded = repo
        .record_generation(&table(&[(1, "root_v2"), (2, "helper")]))
        .unwrap();
    assert!(
        recorded.is_some(),
        "modification must be recorded despite stale clock"
    );

    let idx = repo.materialize_index().unwrap();
    let view = idx.view(intro(1)).unwrap().unwrap();
    assert_eq!(
        view.name(),
        "root_v2",
        "channel must contain the modified payload"
    );
}

// -----------------------------------------------------------------------
// O(delta) write-path tests
// -----------------------------------------------------------------------

/// **Core O(delta) write invariant.**
///
/// Build a 3-symbol package (N ≥ 3), record gen A, then change exactly ONE
/// symbol and record gen B.  The second `record_generation` call MUST NOT
/// perform a whole-tree `output_repository_no_pending` — the WC was already
/// current after gen A, so `working_copy_tip` matches the channel tip and
/// the baseline-resync is skipped.
///
/// Verification via `sync_output_count()`:
/// - Gen A may or may not call sync depending on whether the empty channel's
///   tip happens to equal `[0u8; 32]`.  We capture the count after gen A.
/// - Gen B must NOT increase the count (working_copy_tip is current).
/// - Gen C (another change) must also NOT increase the count.
///
/// We also verify correctness: `materialize` after gen C returns all three
/// symbols with updated names.
#[test]
fn record_generation_write_path_is_o_delta() {
    let repo = IrRepository::in_memory(pkg(), "main").unwrap();

    let gen_a = table(&[(1, "alpha"), (2, "beta"), (3, "gamma")]);
    repo.record_generation(&gen_a)
        .unwrap()
        .expect("gen A must produce a change");

    // Capture count after gen A: the first recording on a fresh repo may or
    // may not resync (depends on whether the empty channel tip == [0;32]).
    // What matters is that subsequent recordings do NOT resync.
    let count_after_a = repo.sync_output_count();

    // Gen B: only symbol 2 changes.  working_copy_tip now matches the gen A
    // tip, so no resync should occur.
    let gen_b = table(&[(1, "alpha"), (2, "beta_v2"), (3, "gamma")]);
    repo.record_generation(&gen_b)
        .unwrap()
        .expect("gen B must produce a change");

    assert_eq!(
        repo.sync_output_count(),
        count_after_a,
        "gen B must NOT trigger a whole-tree sync (working_copy_tip was current after gen A)"
    );

    // Gen C: change a different symbol.  Still no resync expected.
    let gen_c = table(&[(1, "alpha"), (2, "beta_v2"), (3, "gamma_v2")]);
    repo.record_generation(&gen_c)
        .unwrap()
        .expect("gen C must produce a change");

    assert_eq!(
        repo.sync_output_count(),
        count_after_a,
        "gen C must NOT trigger a whole-tree sync either"
    );

    // Correctness: materialize should reflect gen C.
    let mat = repo.materialize().unwrap();
    assert_eq!(mat.live_entries().count(), 3, "all three symbols survive");
    assert!(mat.is_live(intro(1)));
    assert!(mat.is_live(intro(2)));
    assert!(mat.is_live(intro(3)));
    // Symbol 2 must be updated (from gen B).
    let p2 = mat.get(intro(2)).unwrap();
    assert_eq!(
        p2.symbol.name, "beta_v2",
        "symbol 2 must have updated name from gen B"
    );
    // Symbol 3 must be updated (from gen C).
    let p3 = mat.get(intro(3)).unwrap();
    assert_eq!(
        p3.symbol.name, "gamma_v2",
        "symbol 3 must have updated name from gen C"
    );
}

/// **Stale WC path (durable re-open).**
///
/// Record a generation through an on-disk `IrRepository`, drop it, then
/// re-open the same root in a new `IrRepository` (fresh empty WC, but the
/// pristine + changestore persist).  The first `record_generation` on the
/// re-opened repository must perform one whole-tree resync (WC is empty but
/// the channel is non-empty), then leave `working_copy_tip` current so that
/// the SECOND recording skips the sync.
///
/// Correctness: the final `materialize` must reflect all recorded symbols.
#[test]
fn record_generation_stale_wc_resyncs_once() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let pkg = PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("testcrate"));

    // --- first open: record two generations then drop. ---
    {
        use libpijul::changestore::filesystem::FileSystem as FsChanges;

        let repo: IrRepository<FsChanges> = IrRepository::open(root, pkg.clone(), "main").unwrap();
        repo.record_generation(&table_pkg(&pkg, &[(1, "a"), (2, "b"), (3, "c")]))
            .unwrap()
            .expect("gen A");
    }

    // --- second open: fresh IrRepository with empty WC. ---
    {
        use libpijul::changestore::filesystem::FileSystem as FsChanges;

        let repo: IrRepository<FsChanges> = IrRepository::open(root, pkg.clone(), "main").unwrap();

        // WC is empty, channel tip is non-empty → stale.
        // The first record_generation must do ONE whole-tree sync, then update
        // working_copy_tip so subsequent recordings can skip it.
        repo.record_generation(&table_pkg(&pkg, &[(1, "a"), (2, "b_new"), (3, "c")]))
            .unwrap()
            .expect("gen B on re-open");

        assert_eq!(
            repo.sync_output_count(),
            1,
            "re-opened repo must resync once (stale WC path)"
        );

        // A second recording must NOT resync (WC is now current).
        repo.record_generation(&table_pkg(&pkg, &[(1, "a"), (2, "b_new"), (3, "c_new")]))
            .unwrap()
            .expect("gen C");

        assert_eq!(
            repo.sync_output_count(),
            1,
            "second recording after re-open must NOT resync again"
        );

        // Correctness: latest materialize has all three symbols with updated names.
        let mat = repo.materialize().unwrap();
        assert_eq!(mat.live_entries().count(), 3);
        let p3 = mat.get(intro(3)).unwrap();
        assert_eq!(p3.symbol.name, "c_new");
    }
}

/// Helper: build a `PayloadTable` tied to `pkg` (for durable-repo tests).
fn table_pkg(_pkg: &PackageLineageId, entries: &[(u8, &str)]) -> PayloadTable {
    table(entries)
}
