#![cfg(feature = "server")]
//! Red-first specification: **P3 — dual-write a `GenerationRoot` alongside
//! today's opaque `ir_ref`** (`docs/IR-STORAGE-PLAN.md` §6, phase P3).
//!
//! # What P3 is
//!
//! Today a package's IR is stored as **one opaque object**. `BlobBuilder::set_ir`
//! (`index/blob/creation.rs:108-114`) takes a single `Bytes` and addresses the
//! whole thing under one `ir_ref` (`blob/mod.rs:57`). One declaration changes
//! anywhere in the package and the entire object is re-stored and re-fetched.
//!
//! P2 built the replacement: `nudox_ir::generation::GenerationRoot`, a sorted
//! list of `(IntroId, ContentBlake3)` plus each entry's per-generation location.
//! P3 makes the emit path *write* it — **alongside** `ir_ref`, not instead of
//! it. That is what keeps this phase additive and low-risk: every existing
//! reader still finds the object it expects at the hash it expects, and nothing
//! reads the root yet. P4 is where readers move and `ir_ref` retires.
//!
//! # The claim this file actually tests
//!
//! The plan's entire economic argument is that an unchanged entry costs
//! nothing. That is not a design opinion, it is a measurable property of the
//! emit path, and `emit` already reports exactly the right observable:
//! `Emitted { written, deduped }` (`blob/emit.rs:32-38`) counts objects
//! *physically written* versus found already present.
//!
//! So the tests below assert on **physical writes**, not on manifest shape:
//!
//! * re-emitting an unchanged package writes **zero** new entry payloads;
//! * changing one declaration writes **exactly one**;
//! * *moving* a declaration writes **zero**.
//!
//! That last one is the whole point of task #17. `entry_storage_hash` excludes
//! `sym.source`/`sym.span` because including them reported **75.6%** of a real
//! adjacent-version pair as modified when only **1.3%** genuinely was — a 58×
//! inflation from code sliding down a file. If moved entries re-store here, that
//! measurement was for nothing, and a deployment looks catastrophic for reasons
//! having nothing to do with real edits.
//!
//! # Why the generation stamp must NOT change
//!
//! `BlobManifest::identity_bytes` (`blob/mod.rs:116`) answers "is this the same
//! snapshot as before?" and is deliberately kept distinct from the storage
//! address (the file's own comment: "TWO DELIBERATELY DISTINCT HASHES — do not
//! unify them"). Adding a root must not perturb it: the root is derived from
//! the same content `ir_ref` already covers, so folding it in adds no
//! information and would re-stamp every package in the corpus for a purely
//! additive change. `adding_a_root_does_not_change_the_generation_stamp` pins
//! that. The root joins the identity in **P4**, when `ir_ref` leaves it.
//!
//! **Do not weaken these tests to make them pass.** In particular, do not make
//! `set_ir` optional to satisfy the dual-write tests — P3 writes *both*, and a
//! phase that quietly became a cutover is a format break wearing P3's label.

use bytes::Bytes;
use std::path::PathBuf;
use std::sync::Arc;

use ir::change::hash::{ContentBlake3, IntroId};
use ir::change::ids::{EcosystemId, PackageLineageId, PackageName};
use ir::content::entry_storage_hash;
use ir::entry::{Entry, Node, Symbol, Visibility};
use ir::generation::{GenerationRoot, RootEntry};
use ir::index::RawRef;
use ir::kind::Kind;
use ir::kinds::Module;
use object_store::ObjectStore;

mod common;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn symbol(name: &str, docs: &str, source: &str, span: std::ops::Range<usize>) -> Symbol {
    Symbol {
        name: name.to_owned(),
        visibility: Visibility::Public,
        documentation: docs.to_owned(),
        source: PathBuf::from(source),
        span,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    }
}

fn entry(name: &str, docs: &str, source: &str, span: std::ops::Range<usize>) -> Entry {
    Entry::new(
        symbol(name, docs, source, span),
        Node::build(None::<RawRef>, []),
        Kind::Module(Module),
    )
}

fn intro(tag: &str) -> IntroId {
    IntroId::from_domain("nudox.test.intro", tag.as_bytes())
}

fn lineage() -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("acme"))
}

fn row(tag: &str, e: &Entry) -> RootEntry {
    RootEntry {
        intro: intro(tag),
        content: entry_storage_hash(e),
        parent: None,
        source: e.sym().source.clone(),
        span: e.sym().span.clone(),
    }
}

/// The payload actually stored for one entry.
///
/// # This MUST be position-free, and that is not a detail
///
/// The payload is addressed by a hash that deliberately **excludes**
/// `sym.source`/`sym.span` (`entry_storage_hash`, task #17). If the stored
/// bytes *include* position, then two generations of a declaration that merely
/// moved share one key while carrying different bytes — a content-addressing
/// violation. The store would then either reject the second write (if it
/// verifies integrity, which it must) or silently keep the stale first copy,
/// leaving every reader of that payload with a span from an older generation.
///
/// An earlier version of this fixture serialized the whole `Entry`, span
/// included, and induced exactly that. It was only survivable because integrity
/// verification had been switched off for these sections — which is the wrong
/// trade in the wrong direction: the CAS invariant `hash == digest(bytes)` is
/// what `verify_blobs`' corruption audit rests on.
///
/// Position is **generation-scoped**: it lives in the root's `RootEntry`
/// (`source`/`span`), which is rewritten every generation anyway, never in the
/// content-addressed body, which is precisely what must stay stable. This is
/// the resolution `ir/model/tests/storage_hash.rs` already recorded — the entry
/// payload dedups; the root carries where each entry was that time.
fn payload(e: &Entry) -> Bytes {
    let positionless = Entry::new(
        symbol(
            &e.sym().name,
            &e.sym().documentation,
            "",
            0..0,
        ),
        Node::build(None::<RawRef>, []),
        Kind::Module(Module),
    );
    Bytes::from(serde_json::to_vec(&positionless).expect("an entry serializes"))
}

/// A package of three declarations, as (root, per-entry payloads).
fn package(entries: &[(&str, Entry)]) -> (GenerationRoot, Vec<(ContentBlake3, Bytes)>) {
    let root = GenerationRoot::build(
        lineage(),
        entries.iter().map(|(tag, e)| row(tag, e)).collect(),
    );
    let payloads = entries
        .iter()
        .map(|(_, e)| (entry_storage_hash(e), payload(e)))
        .collect();
    (root, payloads)
}

fn baseline_entries() -> Vec<(&'static str, Entry)> {
    vec![
        ("a", entry("alpha", "The alpha module.", "src/lib.rs", 0..10)),
        ("b", entry("beta", "The beta module.", "src/lib.rs", 20..30)),
        (
            "c",
            entry("gamma", "The gamma module.", "src/other.rs", 5..15),
        ),
    ]
}

/// A store backed by a real in-memory object store, so idempotence across two
/// `emit` calls is genuine rather than mocked — the reuse tests below are
/// meaningless against a backend that forgets.
async fn store() -> index::Store<heart::connection::Live> {
    let backend: Arc<dyn ObjectStore> = Arc::new(object_store::memory::InMemory::new());
    let store = index::Store::new(backend);
    <index::Store<heart::connection::Cold> as heart::connection::Connect>::connect(store)
        .await
        .expect("in-memory store connects")
}

/// Build one package snapshot: the same three source files every time, plus
/// whatever IR/root is supplied.
fn build(
    root: &GenerationRoot,
    payloads: Vec<(ContentBlake3, Bytes)>,
    ir_bytes: &'static [u8],
) -> (
    index::blob::BlobManifest,
    Vec<index::blob::creation::PendingSection>,
) {
    let mut builder =
        index::blob::BlobBuilder::new(common::sample_package_id(), common::sample_toolchain());
    for i in 0..3 {
        builder
            .push_file(
                format!("src/f{i}.rs").into(),
                Bytes::from(format!("// file {i}\n")),
            )
            .expect("push_file");
    }
    builder.set_ir(Bytes::from_static(ir_bytes)).expect("set_ir");
    builder
        .set_references(&index::blob::ReferenceSet { by_file: Vec::new() })
        .expect("set_references");
    builder
        .set_generation_root(root, payloads)
        .expect("set_generation_root");
    builder.finalize().expect("finalize")
}

// ---------------------------------------------------------------------------
// 1. Dual-write: both shapes present
// ---------------------------------------------------------------------------

/// P3 writes the root **alongside** `ir_ref`. Both must be present — a phase
/// that dropped `ir_ref` would be P4 in disguise and would break every existing
/// reader.
#[tokio::test]
async fn a_manifest_carries_both_the_legacy_ir_ref_and_a_generation_root() {
    let entries = baseline_entries();
    let (root, payloads) = package(&entries);
    let (manifest, sections) = build(&root, payloads, b"ir-section-bytes");

    assert!(
        manifest.root_ref.is_some(),
        "P3 must attach a generation root"
    );
    assert!(
        sections.iter().any(|s| s.hash == manifest.ir_ref),
        "the legacy opaque IR object must still be written"
    );

    let store = store().await;
    index::blob::emit::emit(&store, manifest, sections)
        .await
        .expect("emit succeeds");
}

/// The root is only useful if the payloads it names are actually retrievable.
/// A root pointing at objects nobody stored is a dangling index.
#[tokio::test]
async fn every_entry_named_by_the_root_is_stored_as_its_own_object() {
    let entries = baseline_entries();
    let (root, payloads) = package(&entries);
    let (manifest, sections) = build(&root, payloads, b"ir-section-bytes");

    for entry in &root.entries {
        assert!(
            sections
                .iter()
                .any(|s| s.hash.as_bytes() == entry.content.as_bytes()),
            "entry {:?} is named by the root but no object was queued for it",
            entry.intro
        );
    }

    let store = store().await;
    index::blob::emit::emit(&store, manifest, sections)
        .await
        .expect("emit succeeds");
}

/// `set_ir` stays mandatory through P3. If `finalize` stopped requiring it, a
/// caller could silently ship root-only packages that P3-era readers cannot
/// read at all.
#[tokio::test]
async fn finalize_still_requires_the_legacy_ir_section() {
    let entries = baseline_entries();
    let (root, payloads) = package(&entries);

    let mut builder =
        index::blob::BlobBuilder::new(common::sample_package_id(), common::sample_toolchain());
    builder
        .push_file("src/lib.rs".into(), Bytes::from_static(b"// x\n"))
        .expect("push_file");
    builder
        .set_references(&index::blob::ReferenceSet { by_file: Vec::new() })
        .expect("set_references");
    builder
        .set_generation_root(&root, payloads)
        .expect("set_generation_root");

    assert!(
        builder.finalize().is_err(),
        "a root must not substitute for the IR section during dual-write"
    );
}

/// THE content-addressing invariant, and the one whose absence let a real
/// defect through: **one key, one byte string, forever.**
///
/// A moved declaration keeps its storage hash (that is the entire point of task
/// #17) — so if the payload encoding were position-sensitive, the same key
/// would name two different byte strings across two generations. Nothing in the
/// dual-write tests below would catch that on its own: the reuse test would
/// still see "already present" and report zero writes, while the stored copy
/// quietly kept an older generation's span.
///
/// So this is asserted directly, on the bytes, before any of the economics.
#[test]
fn one_storage_key_always_names_the_same_bytes() {
    let here = entry("beta", "The beta module.", "src/lib.rs", 20..30);
    let moved = entry("beta", "The beta module.", "src/lib.rs", 900..910);
    let elsewhere = entry("beta", "The beta module.", "src/moved.rs", 20..30);

    assert_eq!(
        entry_storage_hash(&here),
        entry_storage_hash(&moved),
        "fixture precondition — the storage hash ignores position"
    );
    assert_eq!(entry_storage_hash(&here), entry_storage_hash(&elsewhere));

    assert_eq!(
        payload(&here),
        payload(&moved),
        "same storage key, different bytes — the payload encoding is \
         position-sensitive, so a relocation would overwrite or contradict an \
         existing object"
    );
    assert_eq!(payload(&here), payload(&elsewhere));

    // …and it must still separate genuinely different content, or the payload
    // has been flattened into uselessness.
    let edited = entry("beta", "Beta, rewritten.", "src/lib.rs", 20..30);
    assert_ne!(entry_storage_hash(&here), entry_storage_hash(&edited));
    assert_ne!(payload(&here), payload(&edited));
}

/// Entry payloads must be stored under the **same verified** content-addressing
/// discipline as every other section. A separate unverified namespace would
/// silence exactly the corruption `server/save/blobs.rs`'s `verify_blobs` audit
/// exists to find — and it would do so for the one section class that is *new*
/// and therefore least proven.
#[tokio::test]
async fn entry_payloads_are_stored_under_verified_content_addressing() {
    let entries = baseline_entries();
    let (root, payloads) = package(&entries);
    let (manifest, sections) = build(&root, payloads, b"ir-section-bytes");

    let store = store().await;
    index::blob::emit::emit(&store, manifest, sections)
        .await
        .expect("emit succeeds");

    // Every payload the root names must come back, through the ordinary
    // verified read path, byte-identical.
    for (tag, e) in &entries {
        let hash = index::ContentHash::from_bytes(*entry_storage_hash(e).as_bytes());
        let fetched = store
            .get_section(hash)
            .await
            .unwrap_or_else(|err| panic!("entry {tag} must be retrievable: {err}"));
        assert_eq!(
            fetched.as_ref(),
            payload(e).as_ref(),
            "entry {tag} came back with different bytes than were stored"
        );
    }
}

// ---------------------------------------------------------------------------
// 2. The economic claim — measured in physical writes
// ---------------------------------------------------------------------------

/// THE test. Re-emitting a byte-identical package must write **no new entry
/// payloads**. This is the whole premise of the plan; if it fails, incremental
/// storage does not exist no matter how good the root's shape is.
#[tokio::test]
async fn re_emitting_an_unchanged_package_writes_nothing_new() {
    let store = store().await;
    let entries = baseline_entries();

    let (root, payloads) = package(&entries);
    let (manifest, sections) = build(&root, payloads, b"ir-section-bytes");
    let first = index::blob::emit::emit(&store, manifest, sections)
        .await
        .expect("first emit succeeds");
    assert!(first.written > 0, "the first emit must store something");

    let (root, payloads) = package(&entries);
    let (manifest, sections) = build(&root, payloads, b"ir-section-bytes");
    let second = index::blob::emit::emit(&store, manifest, sections)
        .await
        .expect("second emit succeeds");

    assert_eq!(
        second.written, 0,
        "re-emitting identical content must be free; {} objects were rewritten",
        second.written
    );
}

/// The incremental claim, stated precisely: one changed declaration costs one
/// stored object, not a whole package.
#[tokio::test]
async fn changing_one_entry_re_stores_exactly_one_payload() {
    let store = store().await;
    let entries = baseline_entries();

    let (root, payloads) = package(&entries);
    let (manifest, sections) = build(&root, payloads, b"ir-section-bytes");
    index::blob::emit::emit(&store, manifest, sections)
        .await
        .expect("first emit succeeds");

    // Exactly one declaration's documentation changes. Everything else — files,
    // references, the other two entries — is byte-identical.
    let mut edited = baseline_entries();
    edited[1].1 = entry("beta", "Beta, rewritten.", "src/lib.rs", 20..30);
    let (root, payloads) = package(&edited);
    // The opaque IR blob necessarily changes too (it covers the whole package);
    // that is exactly the waste P4 removes, and it is why this asserts on the
    // *entry* payloads rather than on `written` wholesale.
    let (manifest, sections) = build(&root, payloads, b"ir-section-bytes-v2");

    let changed_hash = entry_storage_hash(&edited[1].1);
    let queued_entry_writes = sections
        .iter()
        .filter(|s| {
            edited
                .iter()
                .any(|(_, e)| entry_storage_hash(e).as_bytes() == s.hash.as_bytes())
        })
        .count();
    assert_eq!(
        queued_entry_writes, 3,
        "all three entry payloads are still queued — dedup happens in the store"
    );

    let second = index::blob::emit::emit(&store, manifest, sections)
        .await
        .expect("second emit succeeds");

    // The root object, the new opaque IR blob, and exactly one entry payload.
    assert!(
        second.written <= 3,
        "one edited declaration re-stored {} objects; only the changed entry, \
         the root, and the (P4-doomed) opaque blob should be new",
        second.written
    );
    assert!(
        store
            .get_section(index::ContentHash::from_bytes(*changed_hash.as_bytes()))
            .await
            .is_ok(),
        "the changed entry's payload must be retrievable"
    );
}

/// THE task-#17 test, end to end. A declaration that only *moved* must cost
/// nothing: same content hash, same stored object, only its row in the root
/// changes. If this fails, the 58× churn inflation is back.
#[tokio::test]
async fn moving_an_entry_re_stores_nothing() {
    let store = store().await;
    let entries = baseline_entries();

    let (root, payloads) = package(&entries);
    let (manifest, sections) = build(&root, payloads, b"ir-section-bytes");
    index::blob::emit::emit(&store, manifest, sections)
        .await
        .expect("first emit succeeds");

    // Everything slides down the file — byte-identical declarations, new spans.
    let moved = vec![
        (
            "a",
            entry("alpha", "The alpha module.", "src/lib.rs", 900..910),
        ),
        (
            "b",
            entry("beta", "The beta module.", "src/lib.rs", 920..930),
        ),
        (
            "c",
            entry("gamma", "The gamma module.", "src/other.rs", 905..915),
        ),
    ];
    let (moved_root, payloads) = package(&moved);

    // Precondition: the payloads really are identical, so any write below is a
    // genuine failure of position-independence rather than a changed entry.
    for ((_, before), (_, after)) in entries.iter().zip(moved.iter()) {
        assert_eq!(
            entry_storage_hash(before),
            entry_storage_hash(after),
            "fixture precondition — a move must not change the storage hash"
        );
    }
    assert_ne!(
        moved_root.hash(),
        root.hash(),
        "the root itself must change: it records where entries are"
    );

    let (manifest, sections) = build(&moved_root, payloads, b"ir-section-bytes");
    let second = index::blob::emit::emit(&store, manifest, sections)
        .await
        .expect("second emit succeeds");

    // Only the root object is new. No entry payload, and not the opaque blob
    // (its bytes are unchanged in this fixture).
    assert!(
        second.written <= 1,
        "a pure relocation re-stored {} objects — position is leaking into the \
         storage identity again (task #17)",
        second.written
    );
}

/// Content-addressing means two identical declarations share one object. If
/// they did not, the dedup the whole scheme rests on would be decorative.
#[tokio::test]
async fn two_entries_with_identical_content_are_stored_once() {
    let twins = vec![
        ("x", entry("twin", "Identical body.", "src/a.rs", 0..10)),
        ("y", entry("twin", "Identical body.", "src/b.rs", 40..50)),
    ];
    assert_eq!(
        entry_storage_hash(&twins[0].1),
        entry_storage_hash(&twins[1].1),
        "fixture precondition — identical content dedups"
    );

    let (root, payloads) = package(&twins);
    let (manifest, sections) = build(&root, payloads, b"ir-section-bytes");

    let store = store().await;
    let emitted = index::blob::emit::emit(&store, manifest, sections)
        .await
        .expect("emit succeeds");

    assert!(
        emitted.deduped >= 1,
        "the shared payload must be recognised as already-present, not written \
         twice"
    );
    assert_eq!(
        root.missing(|_| false).len(),
        1,
        "and the root must name it once"
    );
}

// ---------------------------------------------------------------------------
// 3. Additive — nothing existing may shift
// ---------------------------------------------------------------------------

/// P3 is additive. The generation stamp answers "is this the same snapshot?",
/// and the root is derived from content `ir_ref` already covers — so folding it
/// in would add no information while re-stamping every package in the corpus.
#[tokio::test]
async fn adding_a_root_does_not_change_the_generation_stamp() {
    let entries = baseline_entries();
    let (root, payloads) = package(&entries);

    let mut without =
        index::blob::BlobBuilder::new(common::sample_package_id(), common::sample_toolchain());
    without
        .push_file("src/lib.rs".into(), Bytes::from_static(b"// x\n"))
        .expect("push_file");
    without
        .set_ir(Bytes::from_static(b"ir-section-bytes"))
        .expect("set_ir");
    without
        .set_references(&index::blob::ReferenceSet { by_file: Vec::new() })
        .expect("set_references");
    let (plain, _) = without.finalize().expect("finalize");

    let mut with =
        index::blob::BlobBuilder::new(common::sample_package_id(), common::sample_toolchain());
    with.push_file("src/lib.rs".into(), Bytes::from_static(b"// x\n"))
        .expect("push_file");
    with.set_ir(Bytes::from_static(b"ir-section-bytes"))
        .expect("set_ir");
    with.set_references(&index::blob::ReferenceSet { by_file: Vec::new() })
        .expect("set_references");
    with.set_generation_root(&root, payloads)
        .expect("set_generation_root");
    let (rooted, _) = with.finalize().expect("finalize");

    assert_eq!(
        plain.identity_bytes(),
        rooted.identity_bytes(),
        "attaching a root must not re-stamp the generation — the root joins the \
         identity in P4, when `ir_ref` leaves it"
    );
}

/// A manifest written before P3 must still decode. The index and its readers
/// are deployed separately; an older manifest becoming unreadable would be a
/// format break, which P3 explicitly is not.
#[test]
fn a_manifest_without_a_root_still_decodes() {
    let entries = baseline_entries();
    let (root, payloads) = package(&entries);
    let (manifest, _) = build(&root, payloads, b"ir-section-bytes");

    let mut json: serde_json::Value =
        serde_json::to_value(&manifest).expect("manifest serializes");
    json.as_object_mut()
        .expect("manifest is an object")
        .remove("root_ref");

    let decoded: index::blob::BlobManifest =
        serde_json::from_value(json).expect("a pre-P3 manifest must still decode");
    assert!(decoded.root_ref.is_none());
    assert_eq!(decoded.ir_ref, manifest.ir_ref);
}
