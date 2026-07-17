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
