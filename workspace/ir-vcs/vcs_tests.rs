//! Gate tests for the libpijul-backed IR VCS.
//!
//! These prove the whole pivot works end-to-end: a `PristineIntroTable` records
//! into libpijul, materializes back identically, accumulates history, unrecords,
//! and seals a deterministic archive — with libpijul owning changes/deps/unrecord.

use ir::change::{
    EcosystemId, IntroId, PackageLineageId, PackageName, StableRef,
};
use ir::apply::{LinkRecord, PristineIntroTable};
use ir::kind::KindDiscriminant;
use ir::symbol::Visibility;
use ir::wire::{
    EntryPayloadFlags, FunctionWire, KindWire, ModuleWire, OwnedEntryPayload, SymbolWire,
};

use crate::refs::{BranchName, Ref, RefKind, TagName};
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
    }
}

fn function(name: &str) -> OwnedEntryPayload {
    OwnedEntryPayload::sealed(
        sym(name),
        KindDiscriminant::Function,
        KindWire::Function(FunctionWire { input_params: Box::new([]), output_params: Box::new([]), sig: Default::default(), generics: Box::new([]), wheres: Box::new([]) }),
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
/// to it.
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
        mat.get(intro(2)),
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
    let sv = crate::f1::F1View::from_bytes(raw).expect("blob parses");
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
        matches!(repo.tag_version(&vlabel("1.0.0")), Err(crate::VcsError::RefAlreadyExists { .. })),
        "re-tagging the same version must error"
    );
    assert!(
        matches!(repo.materialize_version(&vlabel("9.9.9")), Err(crate::VcsError::RefNotFound { .. })),
        "serving an untagged version must error"
    );
    assert!(
        matches!(repo.version_state(&vlabel("9.9.9")), Err(crate::VcsError::RefNotFound { .. })),
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
    let view = crate::f1::F1View::from_bytes(&raw).unwrap();
    assert_eq!(view.name(), "b", "v1 checkout is the original payload");

    // On main it's `b_v2`.
    let raw_main = repo.checkout_symbol(intro(2)).unwrap().unwrap();
    assert_eq!(crate::f1::F1View::from_bytes(&raw_main).unwrap().name(), "b_v2");

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
    assert_eq!(crate::f1::F1View::from_bytes(&raw).unwrap().name(), "a");

    // Per-symbol history survives too (symbol 2 changed once after creation).
    assert_eq!(repo2.symbol_history(intro(2)).unwrap().len(), 2);
}

// ===========================================================================
// Unified reference model — branches / tags / versions / changes
// ===========================================================================

fn branch(s: &str) -> BranchName {
    BranchName::new(s).unwrap()
}

fn tag(s: &str) -> TagName {
    TagName::new(s).unwrap()
}

/// **The load-bearing branch invariant: switching branches isolates the working
/// copy.** Recording onto a branch must never leak symbols from whatever branch
/// was recorded last — the scratch WC is reset on switch.
#[test]
fn switch_branch_isolates_working_copy() {
    let mut repo = IrRepository::in_memory(pkg(), "main").unwrap();

    // main: symbols 1, 2.
    repo.record_generation(&func_table(&[(1, "a"), (2, "b")])).unwrap().unwrap();

    // Fork develop from main, switch to it, and diverge: change 2, add 3.
    repo.fork_branch(&Ref::Branch(branch("main")), &branch("develop")).unwrap();
    repo.switch_branch(&branch("develop")).unwrap();
    assert_eq!(repo.current_branch().as_str(), "develop");
    repo.record_generation(&func_table(&[(1, "a"), (2, "b_dev"), (3, "c_dev")])).unwrap().unwrap();

    // Switch back to main and diverge differently: change 2 → b_main, no 3.
    repo.switch_branch(&branch("main")).unwrap();
    repo.record_generation(&func_table(&[(1, "a"), (2, "b_main")])).unwrap().unwrap();

    // main must be {1, 2=b_main} with NO leaked symbol 3 from develop.
    let m = repo.materialize_ref(&Ref::Branch(branch("main"))).unwrap();
    assert_eq!(m.len(), 2, "main must not have leaked develop's symbol 3");
    assert_eq!(payload_name(&m, intro(2)), "b_main");
    assert!(m.get(intro(3)).is_none(), "symbol 3 belongs to develop only");

    // develop must be {1, 2=b_dev, 3}.
    let d = repo.materialize_ref(&Ref::Branch(branch("develop"))).unwrap();
    assert_eq!(d.len(), 3);
    assert_eq!(payload_name(&d, intro(2)), "b_dev");
    assert!(d.get(intro(3)).is_some());

    // The two branches genuinely diverged.
    let diff = repo
        .diff_refs(&Ref::Branch(branch("main")), &Ref::Branch(branch("develop")))
        .unwrap();
    assert_eq!(diff.added, vec![intro(3)], "develop adds 3");
    assert_eq!(diff.modified, vec![intro(2)], "2 differs between branches");
}

/// Branch lifecycle: create / fork / list / exists / rename / delete, with the
/// current working branch protected.
#[test]
fn branch_lifecycle() {
    let mut repo = IrRepository::in_memory(pkg(), "main").unwrap();
    repo.record_generation(&func_table(&[(1, "a")])).unwrap().unwrap();

    // The working branch exists implicitly once recorded.
    assert!(repo.branch_exists(&branch("main")).unwrap());
    assert_eq!(repo.current_branch(), branch("main"));

    // Fork + empty create.
    repo.fork_branch(&Ref::Branch(branch("main")), &branch("release/2.x")).unwrap();
    repo.create_branch(&branch("feature/x")).unwrap();
    assert!(repo.branch_exists(&branch("release/2.x")).unwrap());
    assert!(repo.branch_exists(&branch("feature/x")).unwrap());

    // Strictness: re-create errors.
    assert!(matches!(
        repo.create_branch(&branch("feature/x")),
        Err(crate::VcsError::RefAlreadyExists { .. })
    ));

    // Listing partitions only branches (no tag/version leakage).
    repo.tag_version(&vlabel("1.0.0")).unwrap();
    repo.create_tag(&tag("rc1"), &Ref::Branch(branch("main"))).unwrap();
    let mut branches: Vec<String> = repo.list_branches().unwrap().iter().map(|b| b.as_str().to_owned()).collect();
    branches.sort();
    assert_eq!(branches, vec!["feature/x", "main", "release/2.x"]);

    // Rename a non-current branch.
    repo.rename_branch(&branch("feature/x"), &branch("feature/y")).unwrap();
    assert!(!repo.branch_exists(&branch("feature/x")).unwrap());
    assert!(repo.branch_exists(&branch("feature/y")).unwrap());

    // Delete a non-current branch.
    repo.delete_branch(&branch("feature/y")).unwrap();
    assert!(!repo.branch_exists(&branch("feature/y")).unwrap());

    // The current working branch is protected from delete + rename.
    assert!(matches!(
        repo.delete_branch(&branch("main")),
        Err(crate::VcsError::CurrentBranchProtected { .. })
    ));
    assert!(matches!(
        repo.rename_branch(&branch("main"), &branch("trunk")),
        Err(crate::VcsError::CurrentBranchProtected { .. })
    ));

    // Switching to a non-existent branch errors.
    assert!(matches!(
        repo.switch_branch(&branch("ghost")),
        Err(crate::VcsError::RefNotFound { .. })
    ));
}

/// Tags are first-class: create from any ref, list, resolve state, materialize,
/// and delete — and a tag frozen from a branch survives that branch advancing.
#[test]
fn tags_are_first_class() {
    let repo = IrRepository::in_memory(pkg(), "main").unwrap();
    repo.record_generation(&func_table(&[(1, "a"), (2, "b")])).unwrap().unwrap();

    // Tag main's current state.
    let rc_state = repo.create_tag(&tag("rc1"), &Ref::Branch(branch("main"))).unwrap();
    assert!(repo.tag_exists(&tag("rc1")).unwrap());
    assert_eq!(repo.tag_state(&tag("rc1")).unwrap().to_bytes(), rc_state.to_bytes());

    // main advances; the tag stays frozen.
    repo.record_generation(&func_table(&[(1, "a"), (2, "b2"), (3, "c")])).unwrap().unwrap();

    let tagged = repo.materialize_ref(&Ref::Tag(tag("rc1"))).unwrap();
    assert_eq!(tagged.len(), 2, "tag is frozen at 2 symbols");
    assert_eq!(payload_name(&tagged, intro(2)), "b", "tag keeps the original payload");

    // A version can be cut from a tag (release the RC as 1.0.0).
    repo.create_version_from(&vlabel("1.0.0"), &Ref::Tag(tag("rc1"))).unwrap();
    let v = repo.materialize_ref(&Ref::version("1.0.0").unwrap()).unwrap();
    assert_eq!(payload_name(&v, intro(2)), "b", "1.0.0 inherits the RC's state");

    // Listing + deletion.
    assert_eq!(repo.list_tags().unwrap(), vec![tag("rc1")]);
    repo.delete_tag(&tag("rc1")).unwrap();
    assert!(!repo.tag_exists(&tag("rc1")).unwrap());
    assert!(matches!(
        repo.delete_tag(&tag("rc1")),
        Err(crate::VcsError::RefNotFound { .. })
    ));
}

/// A bare change reference is first-class for identity but is not directly
/// servable (it is a point in history, not a channel tip).
#[test]
fn change_ref_is_not_servable() {
    let repo = IrRepository::in_memory(pkg(), "main").unwrap();
    let h = repo.record_generation(&func_table(&[(1, "a")])).unwrap().unwrap();

    let change_ref = Ref::Change(h.clone());
    assert_eq!(change_ref.kind(), RefKind::Change);
    assert!(!change_ref.is_channel_backed());
    assert!(matches!(
        repo.materialize_ref(&change_ref),
        Err(crate::VcsError::RefNotServable { .. })
    ));
    assert!(matches!(
        repo.resolve_ref(&change_ref),
        Err(crate::VcsError::RefNotServable { .. })
    ));
}

/// Unified serving: `serve_ref_cached`, `checkout_symbol_at_ref`, and
/// `symbol_history_on` all work uniformly across branch and tag references.
#[test]
fn unified_ref_serving() {
    use std::num::NonZeroU32;

    let repo = IrRepository::in_memory(pkg(), "main").unwrap();
    repo.record_generation(&func_table(&[(1, "a"), (2, "b")])).unwrap().unwrap();
    repo.create_tag(&tag("snap"), &Ref::Branch(branch("main"))).unwrap();
    repo.record_generation(&func_table(&[(1, "a_v2"), (2, "b")])).unwrap().unwrap();

    // checkout one symbol at the tag vs the branch tip.
    let at_tag = repo.checkout_symbol_at_ref(&Ref::Tag(tag("snap")), intro(1)).unwrap().unwrap();
    assert_eq!(crate::f1::F1View::from_bytes(&at_tag).unwrap().name(), "a");
    let at_branch = repo.checkout_symbol_at_ref(&Ref::Branch(branch("main")), intro(1)).unwrap().unwrap();
    assert_eq!(crate::f1::F1View::from_bytes(&at_branch).unwrap().name(), "a_v2");

    // symbol_history_on the branch: symbol 1 created + changed → 2 changes.
    assert_eq!(repo.symbol_history_on(&Ref::Branch(branch("main")), intro(1)).unwrap().len(), 2);
    // On the frozen tag: symbol 1 only created → 1 change.
    assert_eq!(repo.symbol_history_on(&Ref::Tag(tag("snap")), intro(1)).unwrap().len(), 1);

    // serve_ref_cached works for a tag ref.
    let cache = ServeCache::new(8 << 20, NonZeroU32::new(4).unwrap(), NonZeroU32::new(4).unwrap());
    let s = repo.serve_ref_cached(&Ref::Tag(tag("snap")), &cache).unwrap();
    assert_eq!(s.source, ServeSource::SealedAndStored);
    let s2 = repo.serve_ref_cached(&Ref::Tag(tag("snap")), &cache).unwrap();
    assert_eq!(s2.source, ServeSource::Cached);
    assert!(s2.archive.open().unwrap().get().lookup_intro(intro(1)).is_some());

    // changes_between a frozen tag and the advanced branch = the newer change.
    let delta = repo.changes_between_refs(&Ref::Tag(tag("snap")), &Ref::Branch(branch("main"))).unwrap();
    assert_eq!(delta.len(), 1);
}

/// Branches and tags persist across a reopen of the durable store.
#[test]
fn refs_persist_across_reopen() {
    use libpijul::changestore::filesystem::FileSystem as FsChanges;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    {
        let repo: IrRepository<FsChanges> = IrRepository::open(root, pkg(), "main").unwrap();
        repo.record_generation(&func_table(&[(1, "a"), (2, "b")])).unwrap().unwrap();
        repo.fork_branch(&Ref::Branch(branch("main")), &branch("develop")).unwrap();
        repo.create_tag(&tag("rc1"), &Ref::Branch(branch("main"))).unwrap();
        repo.tag_version(&vlabel("1.0.0")).unwrap();
    }

    let repo2: IrRepository<FsChanges> = IrRepository::open(root, pkg(), "main").unwrap();
    assert!(repo2.branch_exists(&branch("develop")).unwrap());
    assert!(repo2.tag_exists(&tag("rc1")).unwrap());
    assert_eq!(repo2.list_versions().unwrap(), vec![vlabel("1.0.0")]);
    // list_branches partitions correctly after reopen.
    let mut bs: Vec<String> = repo2.list_branches().unwrap().iter().map(|b| b.as_str().to_owned()).collect();
    bs.sort();
    assert_eq!(bs, vec!["develop", "main"]);
    assert_eq!(repo2.materialize_ref(&Ref::Tag(tag("rc1"))).unwrap().len(), 2);
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
            let sv = crate::f1::F1View::from_bytes(raw).unwrap();
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

// ===========================================================================
// RecordingSession tests
// ===========================================================================

use crate::session::StagedEntry;

/// Convert a `PristineIntroTable` entry into a `StagedEntry` for session staging.
fn table_entry_to_staged(
    table: &PristineIntroTable,
    intro_id: IntroId,
) -> StagedEntry {
    let payload = table.get(intro_id).unwrap().clone();
    let parent = table.parent_of(intro_id);
    // Collect links where this intro is the canonical owner.
    let package = pkg();
    let self_ref = StableRef::new(package.clone(), intro_id);
    let links: Vec<ir::serialize::LinkWire> = table
        .links()
        .filter(|l| l.a == self_ref || l.b == self_ref)
        .filter_map(|l| {
            // Same canonical-owner rule as record_generation.
            let a_intro = l.a.intro;
            let b_intro = l.b.intro;
            let a_is_local = l.a.package == package;
            let b_is_local = l.b.package == package;
            let owner_intro = if a_is_local && b_is_local {
                if a_intro.as_bytes() <= b_intro.as_bytes() { a_intro } else { b_intro }
            } else if a_is_local {
                a_intro
            } else {
                b_intro
            };
            if owner_intro != intro_id {
                return None;
            }
            // Build LinkWire from this intro's perspective.
            let (kind_self, kind_other, other) = if l.a.intro == intro_id {
                (l.kind_a, l.kind_b, l.b.clone())
            } else {
                (l.kind_b, l.kind_a, l.a.clone())
            };
            Some(ir::serialize::LinkWire { other, kind_self, kind_other })
        })
        .collect();

    StagedEntry {
        stable: StableRef::new(pkg(), intro_id),
        payload,
        parent,
        links,
    }
}

/// **Equivalence property**: `begin_recording` + `stage` + `finish` produces
/// the same materialized IR as `record_generation` on the same table.
#[test]
fn session_equivalence_property() {
    let ir = sample_ir();

    // Repo A: batch record_generation.
    let repo_a = IrRepository::in_memory(pkg(), "main").unwrap();
    repo_a.record_generation(&ir).unwrap().unwrap();

    // Repo B: streaming session, split into two batches.
    let mut repo_b = IrRepository::in_memory(pkg(), "main").unwrap();
    {
        let mut session = repo_b.begin_recording().unwrap();
        let intros: Vec<IntroId> = ir.live_entries().map(|(i, _)| i).collect();
        let (first, second) = intros.split_at(intros.len() / 2 + 1);
        session
            .stage(first.iter().map(|&i| table_entry_to_staged(&ir, i)).collect())
            .unwrap();
        if !second.is_empty() {
            session
                .stage(second.iter().map(|&i| table_entry_to_staged(&ir, i)).collect())
                .unwrap();
        }
        session.finish().unwrap();
    }

    // Both repos must have exactly one change in their log.
    assert_eq!(repo_a.log().unwrap().len(), 1, "record_generation must produce 1 change");
    assert_eq!(repo_b.log().unwrap().len(), 1, "session must produce 1 change");

    // Materialized IR must be content-identical (same payloads, parents, links).
    // Tip fingerprints will differ because change identity includes timestamps/messages.
    let mat_a = repo_a.materialize().unwrap();
    let mat_b = repo_b.materialize().unwrap();
    assert_eq!(
        mat_a.live_entries().count(),
        mat_b.live_entries().count(),
        "same number of live symbols"
    );
    for (i, payload) in mat_a.live_entries() {
        assert_eq!(mat_b.get(i), Some(payload), "payload for intro {:?} differs", i);
        assert_eq!(mat_a.parent_of(i), mat_b.parent_of(i), "parent mismatch for {:?}", i);
    }
    assert_eq!(mat_a.links().count(), mat_b.links().count(), "link count mismatch");
}

/// **Deletion semantics**: symbols not staged in `finish` are deleted.
#[test]
fn session_deletion_semantics() {
    // Start with 3 symbols.
    let mut repo = IrRepository::in_memory(pkg(), "main").unwrap();
    let mut initial = PristineIntroTable::new();
    initial.insert_live(intro(1), module("root"), None);
    initial.insert_live(intro(2), function("alpha"), None);
    initial.insert_live(intro(3), function("beta"), None);
    repo.record_generation(&initial).unwrap().unwrap();

    // New session: stage only intro(1) and intro(2).
    let report = {
        let mut session = repo.begin_recording().unwrap();
        let batch = vec![
            table_entry_to_staged(&initial, intro(1)),
            table_entry_to_staged(&initial, intro(2)),
        ];
        session.stage(batch).unwrap();
        session.finish().unwrap()
    };

    assert_eq!(report.deleted, 1, "intro(3) should be deleted");

    let mat = repo.materialize().unwrap();
    assert_eq!(mat.live_entries().count(), 2, "only 2 symbols remain");
    assert!(mat.is_live(intro(1)));
    assert!(mat.is_live(intro(2)));
    assert!(!mat.is_live(intro(3)), "intro(3) must be deleted");
}

/// **Checkpoint semantics**: a checkpoint commits staged symbols without
/// deleting unstaged ones; `finish` later records deletions.
#[test]
fn session_checkpoint_mid_session() {
    let mut repo = IrRepository::in_memory(pkg(), "main").unwrap();

    // Start: record 3 symbols.
    let mut initial = PristineIntroTable::new();
    initial.insert_live(intro(1), module("root"), None);
    initial.insert_live(intro(2), function("alpha"), None);
    initial.insert_live(intro(3), function("beta"), None);
    repo.record_generation(&initial).unwrap().unwrap();

    // Session: checkpoint after staging intro(1), then stage intro(2), then finish
    // (deleting intro(3)).
    {
        let mut session = repo.begin_recording().unwrap();

        // Stage intro(1) with changed content.
        let mut changed = PristineIntroTable::new();
        changed.insert_live(intro(1), module("root_v2"), None);
        changed.insert_live(intro(2), function("alpha"), None);

        session
            .stage(vec![table_entry_to_staged(&changed, intro(1))])
            .unwrap();
        let cp = session.checkpoint("partial").unwrap();
        assert!(cp.is_some(), "checkpoint must record a change");

        // Stage intro(2) (unchanged).
        session
            .stage(vec![table_entry_to_staged(&changed, intro(2))])
            .unwrap();

        // finish: intro(3) is not staged → deleted.
        session.finish().unwrap();
    }

    let log = repo.log().unwrap();
    assert_eq!(log.len(), 3, "initial record + checkpoint + finish = 3 changes");

    let mat = repo.materialize().unwrap();
    assert_eq!(mat.live_entries().count(), 2, "intro(3) deleted by finish");
    assert!(mat.is_live(intro(1)));
    assert!(mat.is_live(intro(2)));
    assert!(!mat.is_live(intro(3)));
    // intro(1) content updated.
    assert_eq!(mat.get(intro(1)).unwrap().symbol.name, "root_v2");
}

/// **Checkpoint never deletes**: a partial enumeration cannot distinguish
/// "gone" from "not yet emitted", so a checkpoint must leave every unstaged
/// symbol live on the channel. Proven by checkpointing a strict subset,
/// abandoning the session, and materializing: everything survives.
#[test]
fn session_checkpoint_never_deletes() {
    let mut repo = IrRepository::in_memory(pkg(), "main").unwrap();

    let mut initial = PristineIntroTable::new();
    initial.insert_live(intro(1), module("root"), None);
    initial.insert_live(intro(2), function("alpha"), None);
    initial.insert_live(intro(3), function("beta"), None);
    repo.record_generation(&initial).unwrap().unwrap();

    // Stage ONLY intro(1) with changed content, checkpoint, then abandon —
    // no finish ever runs, so the channel state after the checkpoint is the
    // only state to inspect.
    {
        let mut session = repo.begin_recording().unwrap();
        let mut changed = PristineIntroTable::new();
        changed.insert_live(intro(1), module("root_v2"), None);
        session
            .stage(vec![table_entry_to_staged(&changed, intro(1))])
            .unwrap();
        assert!(session.checkpoint("subset").unwrap().is_some());
        session.abandon().unwrap();
    }

    let mat = repo.materialize().unwrap();
    assert_eq!(mat.live_entries().count(), 3, "checkpoint must not delete anything");
    assert!(mat.is_live(intro(2)) && mat.is_live(intro(3)), "unstaged symbols survive");
    assert_eq!(
        mat.get(intro(1)).unwrap().symbol.name,
        "root_v2",
        "the staged symbol IS on the channel after the checkpoint"
    );
}

/// **Crash recovery**: an abandoned session leaves the repo recoverable.
/// `record_generation` with the full set works correctly after `abandon`.
#[test]
fn session_crash_resume() {
    let mut repo = IrRepository::in_memory(pkg(), "main").unwrap();

    let mut initial = PristineIntroTable::new();
    initial.insert_live(intro(1), module("root"), None);
    initial.insert_live(intro(2), function("alpha"), None);
    initial.insert_live(intro(3), function("beta"), None);
    repo.record_generation(&initial).unwrap().unwrap();

    // Begin a session, stage some symbols, then abandon.
    {
        let mut session = repo.begin_recording().unwrap();
        session
            .stage(vec![table_entry_to_staged(&initial, intro(1))])
            .unwrap();
        session.abandon().unwrap();
    }

    // After abandon, record_generation with the full table must succeed.
    let mut updated = initial.clone();
    updated.insert_live(intro(4), function("gamma"), None);
    let result = repo.record_generation(&updated).unwrap();
    assert!(result.is_some(), "record after abandon must succeed and produce a change");

    let mat = repo.materialize().unwrap();
    assert_eq!(mat.live_entries().count(), 4, "all 4 symbols present after recovery");
    assert!(mat.is_live(intro(4)), "newly added symbol gamma present");
}

/// **Idempotent restage**: finishing a session whose staged content is identical
/// to the current tip records no change.
#[test]
fn session_idempotent_restage() {
    let mut repo = IrRepository::in_memory(pkg(), "main").unwrap();
    let ir = sample_ir();
    repo.record_generation(&ir).unwrap().unwrap();

    // Stage the same content — nothing should change.
    let report = {
        let mut session = repo.begin_recording().unwrap();
        let entries: Vec<StagedEntry> = ir
            .live_entries()
            .map(|(i, _)| table_entry_to_staged(&ir, i))
            .collect();
        let stage_report = session.stage(entries).unwrap();
        assert!(stage_report.unchanged > 0, "all entries should be unchanged");
        session.finish().unwrap()
    };

    assert!(report.change.is_none(), "no change should be recorded for identical content");
    assert_eq!(report.deleted, 0, "nothing deleted");
}

/// **Large batch (SMOLVM P2 gate)**: staging ~100 entries in one call exercises
/// the single-txn / single-`list_files` path and must produce the same
/// materialized IR as `record_generation` on the same table.
#[test]
fn session_large_batch_stage() {
    const N: u8 = 100;

    // Build a table of N top-level functions.
    let mut table = PristineIntroTable::new();
    for i in 0..N {
        table.insert_live(intro(i), function(&format!("sym_{i}")), None);
    }

    // Reference: record_generation.
    let repo_ref = IrRepository::in_memory(pkg(), "main").unwrap();
    repo_ref.record_generation(&table).unwrap().unwrap();

    // Session: stage all N entries in a single batch.
    let mut repo_ses = IrRepository::in_memory(pkg(), "main").unwrap();
    let report = {
        let mut session = repo_ses.begin_recording().unwrap();
        let batch: Vec<StagedEntry> = table
            .live_entries()
            .map(|(i, _)| table_entry_to_staged(&table, i))
            .collect();
        assert_eq!(batch.len(), N as usize, "batch has exactly N entries");
        let stage_report = session.stage(batch).unwrap();
        assert_eq!(stage_report.added, N as u64, "all N are new");
        assert_eq!(stage_report.updated, 0);
        assert_eq!(stage_report.unchanged, 0);
        assert!(stage_report.sample.len() <= 5, "sample capped at 5");
        session.finish().unwrap()
    };

    assert!(report.change.is_some(), "a change must be recorded");
    assert_eq!(report.added, N as u64);
    assert_eq!(report.deleted, 0);

    // Materialized IR must match the reference.
    let mat_ref = repo_ref.materialize().unwrap();
    let mat_ses = repo_ses.materialize().unwrap();
    assert_eq!(mat_ref.live_entries().count(), mat_ses.live_entries().count());
    for (i, payload) in mat_ref.live_entries() {
        assert_eq!(mat_ses.get(i), Some(payload), "payload mismatch for intro {:?}", i);
    }
}

/// **Foreign-package rejection (SV-10)**: staging an entry whose
/// `stable.package` differs from the repository's package must return
/// `VcsError::ForeignPackage` before any WC mutation.  A valid entry staged
/// after the rejection must be committed correctly.
#[test]
fn session_foreign_package_rejected() {
    use ir::change::{EcosystemId, PackageName};

    let foreign_pkg = PackageLineageId::new(
        EcosystemId::new("cargo"),
        PackageName::new("other_crate"),
    );

    let mut repo = IrRepository::in_memory(pkg(), "main").unwrap();

    // Build a foreign entry (different package).
    let foreign_intro = intro(42);
    let foreign_entry = StagedEntry {
        stable: StableRef::new(foreign_pkg.clone(), foreign_intro),
        payload: function("foreign_sym"),
        parent: None,
        links: vec![],
    };

    // Build a valid entry (correct package).
    let valid_intro = intro(10);
    let mut valid_table = PristineIntroTable::new();
    valid_table.insert_live(valid_intro, function("valid_sym"), None);
    let valid_entry = table_entry_to_staged(&valid_table, valid_intro);

    {
        let mut session = repo.begin_recording().unwrap();

        // Staging the foreign entry must error with ForeignPackage.
        let err = session.stage(vec![foreign_entry]).unwrap_err();
        assert!(
            matches!(err, crate::VcsError::ForeignPackage { .. }),
            "expected ForeignPackage, got {err:?}"
        );

        // The WC must be untouched: staging the valid entry now succeeds.
        let stage_report = session.stage(vec![valid_entry]).unwrap();
        assert_eq!(stage_report.added, 1, "the valid entry is added");

        let report = session.finish().unwrap();
        assert!(report.change.is_some(), "a change for the valid entry was recorded");
        assert_eq!(report.added, 1);
    }

    // After finish, only the valid symbol exists — the foreign one was never written.
    let mat = repo.materialize().unwrap();
    assert_eq!(mat.live_entries().count(), 1, "only the valid symbol present");
    assert!(mat.is_live(valid_intro), "valid symbol is live");
    assert!(!mat.is_live(foreign_intro), "foreign symbol must not be present");
}

// ===========================================================================
// record_stream end-to-end tests
// ===========================================================================

use crate::stream::{record_stream, StreamPolicy, StreamedRecording};
use heart::content::{ContentHash, JobKey};
use crate::protocol::{FailureKindWire, PhaseWire, ProducerId, SymbolSink, WireEntry, WireLink};

fn stream_job() -> JobKey {
    JobKey::derive(b"stream-test", b"rust-1.79", b"src/lib.rs", b"Cargo.lock")
}

fn stream_producer() -> ProducerId {
    ProducerId::from_static("rust-1.79")
}

fn stream_content_hash(seed: u8) -> ContentHash {
    ContentHash::of_bytes(&[seed; 32])
}

/// Build a `WireEntry` for a function symbol belonging to `pkg()`.
fn wire_entry(name: &str, seed: u8) -> WireEntry {
    use ir::wire::{EntryPayloadFlags, FunctionWire, KindWire, OwnedEntryPayload, SymbolWire};
    use ir::kind::KindDiscriminant;
    use ir::symbol::Visibility;

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
    let kind = KindWire::Function(FunctionWire {
        input_params: Box::new([]),
        output_params: Box::new([]),
        sig: Default::default(),
        generics: Box::new([]),
        wheres: Box::new([]),
    });
    let payload = OwnedEntryPayload::sealed(
        sym,
        KindDiscriminant::Function,
        kind,
        EntryPayloadFlags::default(),
    );
    WireEntry {
        stable: StableRef::new(pkg(), IntroId::from_raw([seed; 32])),
        payload,
        parent: None,
        links: Vec::new(),
    }
}

/// Build a `WireEntry` with a parent set.
fn wire_entry_child(name: &str, seed: u8, parent_seed: u8) -> WireEntry {
    let mut e = wire_entry(name, seed);
    e.parent = Some(IntroId::from_raw([parent_seed; 32]));
    e
}

/// Produce a complete stream into a byte buffer synchronously.
/// Returns the bytes.
fn produce_stream(
    entries: Vec<WireEntry>,
    links: Vec<(StableRef, WireLink)>,
    source_path: Option<&str>,
    finish_or_abort: bool, // true = Finish, false = Abort
) -> Vec<u8> {
    let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let buf2 = buf.clone();

    struct ArcVecWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for ArcVecWriter {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut sink = SymbolSink::hello(ArcVecWriter(buf2), stream_job(), stream_producer()).unwrap();
    sink.emit_all(entries).unwrap();
    for (from, link) in links {
        sink.emit_link(from, link).unwrap();
    }
    if let Some(path) = source_path {
        sink.source_digest(path, stream_content_hash(0xAA), 4242).unwrap();
    }
    sink.progress(PhaseWire::Emit).unwrap();
    if finish_or_abort {
        sink.finish(stream_content_hash(0xFF)).unwrap();
    } else {
        sink.abort(FailureKindWire::Internal, "producer crashed").unwrap();
    }

    buf.lock().unwrap().clone()
}

/// **Happy path**: N entries in several batches + inline links + source digest →
/// `record_stream` → materialized index equals a batch `record_generation` of
/// the same table.
///
/// Links are embedded in `WireEntry.links` (the canonical same-entry path) so
/// they are included in the `Symbols` batch itself. A separate `Links` frame
/// test is covered by `stream_separate_links_frame`.
#[test]
fn stream_happy_path_equivalence() {
    use ir::apply::{LinkRecord, PristineIntroTable};
    use ir::kind::KindDiscriminant;

    let m_seed = 0x10u8;
    let f_seed = 0x20u8;
    let m_intro = IntroId::from_raw([m_seed; 32]);
    let f_intro = IntroId::from_raw([f_seed; 32]);

    // "root" owns the link (m_intro=[0x10] < f_intro=[0x20]).
    let mut root_entry = wire_entry("root", m_seed);
    root_entry.links = vec![WireLink {
        other: StableRef::new(pkg(), f_intro),
        kind_self: KindDiscriminant::Module,
        kind_other: KindDiscriminant::Function,
    }];

    let entries = vec![
        root_entry,
        wire_entry_child("do_thing", f_seed, m_seed),
    ];

    let bytes = produce_stream(entries, vec![], Some("src/lib.rs"), true);

    let mut repo = IrRepository::in_memory(pkg(), "main").unwrap();
    let outcome =
        record_stream(&mut repo, std::io::Cursor::new(bytes), StreamPolicy { checkpoint_every: None })
            .unwrap();

    match &outcome {
        StreamedRecording::Finished { sources, .. } => {
            assert_eq!(sources.len(), 1, "source digest collected");
            assert_eq!(sources[0].path, "src/lib.rs");
        }
        StreamedRecording::Aborted { .. } => panic!("expected Finished"),
    }

    // Build the equivalent PristineIntroTable for reference.
    let mut ir = PristineIntroTable::new();
    ir.insert_live(m_intro, function("root"), None);
    ir.insert_live(f_intro, function("do_thing"), Some(m_intro));
    ir.insert_link(LinkRecord {
        a: sref(m_intro),
        b: sref(f_intro),
        kind_a: KindDiscriminant::Module,
        kind_b: KindDiscriminant::Function,
    });

    let repo_ref = IrRepository::in_memory(pkg(), "main").unwrap();
    repo_ref.record_generation(&ir).unwrap().unwrap();

    let mat_stream = repo.materialize().unwrap();
    let mat_ref = repo_ref.materialize().unwrap();

    assert_eq!(
        mat_stream.live_entries().count(),
        mat_ref.live_entries().count(),
        "same number of live entries"
    );
    for (i, payload) in mat_ref.live_entries() {
        assert_eq!(
            mat_stream.get(i),
            Some(payload),
            "payload mismatch for intro {:?}",
            i
        );
        assert_eq!(
            mat_ref.parent_of(i),
            mat_stream.parent_of(i),
            "parent mismatch for {:?}",
            i
        );
    }
    assert_eq!(
        mat_stream.links().count(),
        mat_ref.links().count(),
        "link count mismatch"
    );
}

/// **Separate Links frame**: a `Links { from, batch }` frame sent AFTER the
/// owning symbol's `Symbols` batch updates the symbol's blob with the link.
///
/// This tests the `stage_links` path in `record_stream`. The producer flushes
/// the symbol batch first (`sink.finish()` drains the buffer), then emits a
/// separate Links frame.
#[test]
fn stream_separate_links_frame() {
    use crate::protocol::{FrameWriter, StreamFrame, IR_STREAM_VERSION};
    use ir::kind::KindDiscriminant;
    use ir::apply::{LinkRecord, PristineIntroTable};

    let m_seed = 0x10u8;
    let f_seed = 0x20u8;
    let m_intro = IntroId::from_raw([m_seed; 32]);
    let f_intro = IntroId::from_raw([f_seed; 32]);

    // Build a stream manually: Hello, Symbols (no links in entries), Links, Finish.
    let mut buf = Vec::<u8>::new();
    let mut fw = FrameWriter::new(&mut buf);
    fw.write_frame(&StreamFrame::Hello {
        version: IR_STREAM_VERSION,
        job: stream_job(),
        producer: stream_producer(),
    }).unwrap();
    fw.write_frame(&StreamFrame::Symbols {
        batch: vec![
            wire_entry("root", m_seed),
            wire_entry_child("do_thing", f_seed, m_seed),
        ],
    }).unwrap();
    // Now emit the link in a separate frame (m_intro owns the link).
    fw.write_frame(&StreamFrame::Links {
        from: StableRef::new(pkg(), m_intro),
        batch: vec![WireLink {
            other: StableRef::new(pkg(), f_intro),
            kind_self: KindDiscriminant::Module,
            kind_other: KindDiscriminant::Function,
        }],
    }).unwrap();
    fw.write_frame(&StreamFrame::Finish {
        emitted: 2,
        producer_digest: stream_content_hash(0xFF),
    }).unwrap();
    let _ = fw;

    let mut repo = IrRepository::in_memory(pkg(), "main").unwrap();
    let outcome = record_stream(
        &mut repo,
        std::io::Cursor::new(buf),
        StreamPolicy { checkpoint_every: None },
    ).unwrap();

    assert!(matches!(outcome, StreamedRecording::Finished { .. }), "must finish");

    // Build reference via record_generation.
    let mut ir = PristineIntroTable::new();
    ir.insert_live(m_intro, function("root"), None);
    ir.insert_live(f_intro, function("do_thing"), Some(m_intro));
    ir.insert_link(LinkRecord {
        a: sref(m_intro),
        b: sref(f_intro),
        kind_a: KindDiscriminant::Module,
        kind_b: KindDiscriminant::Function,
    });

    let repo_ref = IrRepository::in_memory(pkg(), "main").unwrap();
    repo_ref.record_generation(&ir).unwrap().unwrap();

    let mat_stream = repo.materialize().unwrap();
    let mat_ref = repo_ref.materialize().unwrap();

    assert_eq!(
        mat_stream.links().count(),
        mat_ref.links().count(),
        "link count: stream={} vs ref={}",
        mat_stream.links().count(),
        mat_ref.links().count()
    );
    assert_eq!(mat_stream.live_entries().count(), 2, "both symbols present");
}

/// **Checkpoint policy**: `checkpoint_every: 2` with entries arriving in multiple
/// batches ⇒ log length > 1 and content is still correct.
///
/// We produce a stream where each entry arrives in its own 1-entry `Symbols`
/// frame. That way batch 1 stages entry 1, batch 2 stages entry 2 (total=2,
/// multiple=2 > 0 → checkpoint fires), batch 3 stages entry 3, batch 4 stages
/// entry 4 (total=4, multiple=4 > 2 → second checkpoint fires), batch 5 stages
/// entry 5. Then `finish()` completes with a final change (deletions = 0,
/// change recorded since the last checkpoint is already committed; but the 5th
/// entry was staged after the last checkpoint so finish sees pending content).
/// We assert at least 2 changes.
#[test]
fn stream_checkpoint_policy() {
    use crate::protocol::{FrameWriter, StreamFrame, IR_STREAM_VERSION};

    // Build a stream manually: Hello, 5 × single-entry Symbols, Finish.
    let mut buf = Vec::<u8>::new();
    let mut fw = FrameWriter::new(&mut buf);
    fw.write_frame(&StreamFrame::Hello {
        version: IR_STREAM_VERSION,
        job: stream_job(),
        producer: stream_producer(),
    }).unwrap();
    for i in 1u8..=5 {
        fw.write_frame(&StreamFrame::Symbols {
            batch: vec![wire_entry(&format!("sym_{i}"), i)],
        }).unwrap();
    }
    fw.write_frame(&StreamFrame::Finish {
        emitted: 5,
        producer_digest: stream_content_hash(0xFF),
    }).unwrap();
    let _ = fw;

    let mut repo = IrRepository::in_memory(pkg(), "main").unwrap();
    let outcome = record_stream(
        &mut repo,
        std::io::Cursor::new(buf),
        StreamPolicy { checkpoint_every: std::num::NonZeroU64::new(2) },
    )
    .unwrap();

    assert!(matches!(outcome, StreamedRecording::Finished { .. }), "must finish");

    let log = repo.log().unwrap();
    assert!(
        log.len() >= 2,
        "checkpoint_every=2 on 5 single-entry batches must produce >=2 changes, got {}",
        log.len()
    );

    // Content must still be correct: all 5 symbols present.
    let mat = repo.materialize().unwrap();
    assert_eq!(mat.live_entries().count(), 5, "all 5 symbols present");
}

/// **Abort mid-stream**: Aborted outcome, repo tip unchanged, next
/// `record_generation` works (resync path).
#[test]
fn stream_abort_mid_stream() {
    // First record something so the repo tip is non-zero.
    let mut repo = IrRepository::in_memory(pkg(), "main").unwrap();
    let ir = sample_ir();
    repo.record_generation(&ir).unwrap().unwrap();
    let tip_before = repo.tip().unwrap();

    // Now stream an abort.
    let bytes = produce_stream(
        vec![wire_entry("partial", 0x77)],
        vec![],
        None,
        false, // Abort
    );

    let outcome =
        record_stream(&mut repo, std::io::Cursor::new(bytes), StreamPolicy { checkpoint_every: None })
            .unwrap();

    match &outcome {
        StreamedRecording::Aborted { failure, message, .. } => {
            assert_eq!(*failure, FailureKindWire::Internal);
            assert_eq!(message, "producer crashed");
        }
        StreamedRecording::Finished { .. } => panic!("expected Aborted"),
    }

    // Repo tip must be unchanged.
    let tip_after = repo.tip().unwrap();
    assert_eq!(
        tip_before.fingerprint(),
        tip_after.fingerprint(),
        "repo tip must not change on abort"
    );

    // Resync path: a subsequent record_generation must succeed.
    let mut ir2 = ir.clone();
    ir2.insert_live(IntroId::from_raw([0x99; 32]), function("new_thing"), None);
    let change = repo.record_generation(&ir2).unwrap();
    assert!(change.is_some(), "record_generation after abort must produce a change");
    assert_eq!(
        repo.materialize().unwrap().live_entries().count(),
        3,
        "both original + new symbol present"
    );
}

/// **Protocol violation**: truncated transport → `VcsError::Stream`, session
/// abandoned, repo still usable after.
#[test]
fn stream_protocol_violation_truncated() {
    use crate::protocol::{FrameWriter, StreamFrame, IR_STREAM_VERSION};

    let mut repo = IrRepository::in_memory(pkg(), "main").unwrap();
    let ir = sample_ir();
    repo.record_generation(&ir).unwrap().unwrap();
    let tip_before = repo.tip().unwrap();

    // Build a truncated stream: valid Hello + partial frame length header.
    let mut buf = Vec::<u8>::new();
    let mut fw = FrameWriter::new(&mut buf);
    fw.write_frame(&StreamFrame::Hello {
        version: IR_STREAM_VERSION,
        job: stream_job(),
        producer: stream_producer(),
    })
    .unwrap();
    let _ = fw;
    // Append a length header claiming 100 bytes, but no body.
    let fake_len: u32 = 100;
    buf.extend_from_slice(&fake_len.to_le_bytes());
    buf.extend_from_slice(&[0u8; 3]); // only 3 bytes of claimed 100

    let result = record_stream(
        &mut repo,
        std::io::Cursor::new(buf),
        StreamPolicy { checkpoint_every: None },
    );

    assert!(result.is_err(), "truncated stream must return Err");
    let err = result.unwrap_err();
    assert!(
        matches!(err, crate::VcsError::Stream(_)),
        "expected VcsError::Stream, got {err:?}"
    );

    // Repo tip must be unchanged.
    let tip_after = repo.tip().unwrap();
    assert_eq!(
        tip_before.fingerprint(),
        tip_after.fingerprint(),
        "repo tip unchanged after protocol violation"
    );

    // Repo must be usable: a subsequent record_generation must succeed.
    let change = repo.record_generation(&ir).unwrap();
    // The IR is the same as before — may be a no-op, but the repo is not broken.
    let mat = repo.materialize().unwrap();
    assert_eq!(mat.live_entries().count(), 2, "original symbols still present");
    let _ = change;
}

/// **Foreign-package entry in the stream**: `VcsError::ForeignPackage` surfaces
/// through `record_stream` and the repo stays clean.
#[test]
fn stream_foreign_package_rejected() {
    let mut repo = IrRepository::in_memory(pkg(), "main").unwrap();

    // Build a stream with a foreign-package entry.
    let foreign_pkg = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("other"));
    let foreign_ref = StableRef::new(foreign_pkg, IntroId::from_raw([0xEE; 32]));

    use ir::wire::{EntryPayloadFlags, FunctionWire, KindWire, OwnedEntryPayload, SymbolWire};
    use ir::kind::KindDiscriminant;
    use ir::symbol::Visibility;

    let foreign_entry = WireEntry {
        stable: foreign_ref,
        payload: OwnedEntryPayload::sealed(
            SymbolWire {
                name: "foreign".to_owned(),
                visibility: Visibility::Public,
                documentation: None,
                source_path: "index.ts".to_owned(),
                span_start: 0,
                span_end: 7,
                aliases: Vec::new(),
                deprecation: None,
                doc_links: Vec::new(),
                attrs: Vec::new(),
                cfg: None,
            },
            KindDiscriminant::Function,
            KindWire::Function(FunctionWire {
                input_params: Box::new([]),
                output_params: Box::new([]),
                sig: Default::default(),
                generics: Box::new([]),
                wheres: Box::new([]),
            }),
            EntryPayloadFlags::default(),
        ),
        parent: None,
        links: Vec::new(),
    };

    // Produce a stream with just the foreign entry.
    let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));

    struct ArcVecWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for ArcVecWriter {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }

    let mut sink = SymbolSink::hello(ArcVecWriter(buf.clone()), stream_job(), stream_producer()).unwrap();
    sink.emit(foreign_entry).unwrap();
    sink.finish(stream_content_hash(0x01)).unwrap();

    let bytes = buf.lock().unwrap().clone();

    let tip_before = repo.tip().unwrap();
    let result = record_stream(
        &mut repo,
        std::io::Cursor::new(bytes),
        StreamPolicy { checkpoint_every: None },
    );

    assert!(result.is_err(), "foreign package must produce an error");
    let err = result.unwrap_err();
    assert!(
        matches!(err, crate::VcsError::ForeignPackage { .. }),
        "expected ForeignPackage, got {err:?}"
    );

    // Repo tip unchanged — session was abandoned.
    let tip_after = repo.tip().unwrap();
    assert_eq!(
        tip_before.fingerprint(),
        tip_after.fingerprint(),
        "repo tip unchanged after ForeignPackage rejection"
    );

    // Repo is clean: an empty materialize works fine.
    let mat = repo.materialize().unwrap();
    assert_eq!(mat.live_entries().count(), 0, "no symbols after rejected stream");
}
