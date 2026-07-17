//! Adversarial probes for the reference model — edge cases surfaced during an
//! adversarial review of the branch/tag/version/change surface. Every one is a
//! negative result (no bug), kept as a regression guard.

#![cfg(test)]

use nudox_change::{EcosystemId, IntroId, PackageLineageId, PackageName};
use nudox_ir::apply::PristineIntroTable;
use nudox_ir::kind::KindDiscriminant;
use nudox_ir::wire::{EntryPayloadFlags, FunctionWire, KindWire, OwnedEntryPayload, SymbolWire};

use crate::refs::{BranchName, Ref, TagName};
use crate::repo::{ChangeHashHex, IrRepository};
use crate::serve_cache::ServeCache;
use crate::version::VersionLabel;

fn pkg() -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("mylib"))
}
fn intro(n: u8) -> IntroId {
    IntroId::from_raw([n; 32])
}
fn func(name: &str) -> OwnedEntryPayload {
    let sym = SymbolWire {
        name: name.to_owned(),
        visibility: 0,
        documentation: None,
        source_path: "src/lib.rs".to_owned(),
        span_start: 0,
        span_end: name.len() as u32,
        aliases: Vec::new(),
        deprecation: None,
        doc_links: Vec::new(),
    };
    OwnedEntryPayload::sealed(
        sym,
        KindDiscriminant::Function,
        KindWire::Function(FunctionWire { input_params: Box::new([]), output_params: Box::new([]) }),
        EntryPayloadFlags::default(),
    )
}
fn table(entries: &[(u8, &str)]) -> PristineIntroTable {
    let mut t = PristineIntroTable::new();
    for (n, name) in entries {
        t.insert_live(intro(*n), func(name), None);
    }
    t
}
fn br(s: &str) -> BranchName {
    BranchName::new(s).unwrap()
}
fn tg(s: &str) -> TagName {
    TagName::new(s).unwrap()
}
fn vl(s: &str) -> VersionLabel {
    VersionLabel::new(s).unwrap()
}

/// A version/fork from an empty branch (tip = the empty state) materializes to
/// zero symbols without error.
#[test]
fn version_from_empty_branch() {
    let repo = IrRepository::in_memory(pkg(), "main").unwrap();
    repo.create_branch(&br("empty")).unwrap();
    repo.create_version_from(&vl("0.0.0"), &Ref::Branch(br("empty"))).unwrap();
    let idx = repo.materialize_version(&vl("0.0.0")).unwrap();
    assert_eq!(idx.len(), 0, "empty version materializes to zero symbols");
}

/// The empty-channel tip is the Ed25519 basepoint, NOT `[0; 32]`. This is why
/// `record_generation`'s `working_copy_tip` sentinel (`[0; 32]`) is safe: it can
/// never equal a real channel tip, so the first record always resyncs.
#[test]
fn empty_state_is_basepoint_not_zero_sentinel() {
    let repo = IrRepository::in_memory(pkg(), "main").unwrap();
    repo.create_branch(&br("e1")).unwrap();
    repo.create_branch(&br("e2")).unwrap();
    let s1 = repo.resolve_ref(&Ref::Branch(br("e1"))).unwrap().state.to_bytes();
    let s2 = repo.resolve_ref(&Ref::Branch(br("e2"))).unwrap().state.to_bytes();
    assert_eq!(s1, s2, "two empty channels share the same tip");
    assert_ne!(s1, [0u8; 32], "empty tip is the Ed25519 basepoint, never the [0;32] sentinel");
}

/// Cutting a version from a tag (frozen → frozen) inherits the tag's exact state.
#[test]
fn version_from_tag_source() {
    let repo = IrRepository::in_memory(pkg(), "main").unwrap();
    repo.record_generation(&table(&[(1, "a"), (2, "b")])).unwrap().unwrap();
    let tag_state = repo.create_tag(&tg("t1"), &Ref::Branch(br("main"))).unwrap();
    let ver_state = repo.create_version_from(&vl("1.0.0"), &Ref::Tag(tg("t1"))).unwrap();
    assert_eq!(tag_state.to_bytes(), ver_state.to_bytes(), "version from tag shares the tag state");
    assert_eq!(repo.materialize_version(&vl("1.0.0")).unwrap().len(), 2);
}

/// `delete_*` on an absent target is a strict `RefNotFound`, never a silent ok.
#[test]
fn delete_absent_is_strict() {
    let repo = IrRepository::in_memory(pkg(), "main").unwrap();
    assert!(matches!(repo.delete_tag(&tg("nope")), Err(crate::VcsError::RefNotFound { .. })));
    assert!(matches!(repo.delete_version(&vl("9.9.9")), Err(crate::VcsError::RefNotFound { .. })));
    assert!(matches!(repo.delete_branch(&br("nope")), Err(crate::VcsError::RefNotFound { .. })));
}

/// `changes_between_refs` is directional: `(descendant → ancestor)` is empty,
/// `(ancestor → descendant)` is the intervening change.
#[test]
fn changes_between_is_directional() {
    let repo = IrRepository::in_memory(pkg(), "main").unwrap();
    repo.record_generation(&table(&[(1, "a")])).unwrap().unwrap();
    repo.create_tag(&tg("old"), &Ref::Branch(br("main"))).unwrap();
    repo.record_generation(&table(&[(1, "a"), (2, "b")])).unwrap().unwrap();

    let backward = repo.changes_between_refs(&Ref::Branch(br("main")), &Ref::Tag(tg("old"))).unwrap();
    assert!(backward.is_empty(), "old has no change absent from main");
    let forward = repo.changes_between_refs(&Ref::Tag(tg("old")), &Ref::Branch(br("main"))).unwrap();
    assert_eq!(forward.len(), 1, "main has exactly one change not in old");
}

/// A bare change reference resolves to neither a channel nor a materialization.
#[test]
fn change_ref_not_servable() {
    let repo = IrRepository::in_memory(pkg(), "main").unwrap();
    let cref = Ref::Change(ChangeHashHex("deadbeef".to_owned()));
    assert!(matches!(repo.resolve_ref(&cref), Err(crate::VcsError::RefNotServable { .. })));
    assert!(matches!(repo.materialize_ref(&cref), Err(crate::VcsError::RefNotServable { .. })));
}

/// Renaming a branch onto an existing name is a strict `RefAlreadyExists`.
#[test]
fn rename_onto_existing_is_strict() {
    let mut repo = IrRepository::in_memory(pkg(), "main").unwrap();
    repo.record_generation(&table(&[(1, "a")])).unwrap().unwrap();
    repo.create_branch(&br("x")).unwrap();
    repo.create_branch(&br("y")).unwrap();
    repo.switch_branch(&br("y")).unwrap(); // move off main so x/y are both non-current
    assert!(matches!(
        repo.rename_branch(&br("x"), &br("y")),
        Err(crate::VcsError::RefAlreadyExists { .. })
    ));
}

/// **Cache staleness on a movable branch.** The serve cache is keyed by channel
/// *state* (Merkle), not by ref name — so serving a branch, recording more, and
/// serving again yields the NEW content, never a stale hit.
#[test]
fn serve_branch_after_record_is_not_stale() {
    use std::num::NonZeroU32;
    let repo = IrRepository::in_memory(pkg(), "main").unwrap();
    let cache = ServeCache::new(8 << 20, NonZeroU32::new(100).unwrap(), NonZeroU32::new(100).unwrap());

    repo.record_generation(&table(&[(1, "a")])).unwrap().unwrap();
    let s1 = repo.serve_ref_cached(&Ref::Branch(br("main")), &cache).unwrap();
    let y1 = s1.archive.open().unwrap();
    assert!(y1.get().lookup_intro(intro(1)).is_some());
    assert!(y1.get().lookup_intro(intro(2)).is_none());

    repo.record_generation(&table(&[(1, "a"), (2, "b")])).unwrap().unwrap();
    let s2 = repo.serve_ref_cached(&Ref::Branch(br("main")), &cache).unwrap();
    let y2 = s2.archive.open().unwrap();
    assert!(
        y2.get().lookup_intro(intro(2)).is_some(),
        "second serve reflects the new record, not a stale cache hit"
    );
}

/// The serve cache is shareable across threads (its contents are `Send + Sync`),
/// even though `IrRepository` itself is single-threaded (`!Sync` via its `Cell`s).
#[test]
fn serve_cache_is_shareable() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ServeCache>();
    assert_send_sync::<crate::ServedArchive>();
}
