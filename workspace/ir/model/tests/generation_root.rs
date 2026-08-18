//! Red-first specification: **`GenerationRoot`** — the plan's keystone type
//! (`docs/IR-STORAGE-PLAN.md` §1, phase P2).
//!
//! # What this type is for
//!
//! Today a package's IR crosses the wire and lands in storage as **one opaque
//! blob**: `IrSnapshot { package, declarations: Vec<(IntroId, Entry,
//! Option<IntroId>)> }`, serialized whole with `serde_json` and addressed by a
//! single `ContentHash` (`nudox-engine/src/store/remote.rs:47-70`,
//! `publish_ir`/`fetch_ir` at `:168-181`). One declaration changes anywhere in
//! a package and the entire blob is re-stored and re-fetched.
//!
//! `GenerationRoot` is the replacement: a **sorted list of (stable identity,
//! content hash)** plus the per-generation location of each entry. That single
//! structure is simultaneously
//!
//! * the **storage index** — each entry is stored and addressed on its own,
//! * the **diff basis** — two roots linear-merge into added/removed/changed,
//! * the **fault-in manifest** — "which of these hashes do I not have?", and
//! * the **reuse key** — an unchanged entry is literally the same hash.
//!
//! Four mechanisms collapsing into one structure is the whole argument for the
//! plan; this file is where that claim becomes checkable.
//!
//! # The distinction the entire design rests on: *moved* is not *changed*
//!
//! Task #17 established that content identity must be **position-independent**:
//! `entry_storage_hash` deliberately excludes `sym.source`/`sym.span`, because
//! including them inflated apparent churn **58×** on a real package pair
//! (75.6% "modified" vs 1.3%) purely from declarations shifting down a file.
//!
//! But a consumer still has to know *where* each declaration is. The resolution
//! recorded in `tests/storage_hash.rs` is that **location is generation-scoped,
//! not content-scoped**: it lives here, in the root, which is rewritten every
//! generation anyway, rather than inside the content-addressed payload, which
//! is precisely what must stay stable.
//!
//! That makes a `moved` classification not merely nice-to-have but the point:
//! an entry that only moved has the **same content hash**, so it needs no
//! refetch and no re-store — only its row in the root is rewritten. A diff that
//! reported those as `changed` would hand the fault-in path the 75.6% figure
//! and undo the entire measured win. `a_move_is_classified_as_moved_not_changed`
//! is the pin.
//!
//! Tree edges get the same treatment for the same reason, and this is already
//! true in the implementation rather than a thing to add: `entry_storage_hash`
//! excludes the `Node`'s parent and children (`content/mod.rs:403-405`). So
//! reparenting a declaration is a **move**, not a content change, and the
//! parent edge belongs in the root beside the location. This settles plan open
//! question 3 ("where do parent edges live — a third tuple element, or inside
//! the hashed payload?") in favour of the root, on the evidence of code that
//! already exists.
//!
//! # Canonicity is a correctness property, not tidiness
//!
//! The root's hash is a package's identity — "has this package changed" becomes
//! "is this the same root hash". That only works if the encoding is
//! **bijective**: if two distinct byte strings could decode to the same logical
//! root, or one logical root could encode two ways, the hash stops identifying
//! anything. So canonical ordering is enforced on **decode**, not merely
//! produced on encode (`decode_rejects_unsorted_entries`,
//! `decode_rejects_duplicate_intro_ids`) — a rule that is free to state now and
//! very expensive to retrofit once roots are on disk.
//!
//! **Do not weaken these tests to make them pass.** In particular, do not make
//! `diff` classify moves as changes to simplify the merge, and do not drop the
//! decode-side canonicity checks.

use std::collections::BTreeSet;
use std::path::PathBuf;

use nudox_ir::change::hash::IntroId;
use nudox_ir::change::ids::{EcosystemId, PackageLineageId, PackageName};
use nudox_ir::content::entry_storage_hash;
use nudox_ir::entry::{Entry, Node, Symbol, Visibility};
use nudox_ir::generation::{EntryChange, GenerationRoot, RootEntry};
use nudox_ir::index::RawRef;
use nudox_ir::kind::Kind;
use nudox_ir::kinds::Module;

// ---------------------------------------------------------------------------
// Fixtures
//
// Built from the crate's public constructors only (`nudox_ir::test_helpers` is
// crate-private and unreachable from an integration test) — the same approach
// `tests/storage_hash.rs` takes, for the same reason.
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

/// A deterministic `IntroId` for a fixture, so tests can name entries stably.
/// `IntroId` wraps a `ContentBlake3`, so this derives one from a tag string.
fn intro(tag: &str) -> IntroId {
    IntroId::from_domain("nudox.test.intro", tag.as_bytes())
}

fn lineage() -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("acme"))
}

/// One root row: identity, content hash derived from a real `Entry`, plus the
/// generation-scoped location and tree edge.
fn row(tag: &str, e: &Entry, parent: Option<IntroId>) -> RootEntry {
    RootEntry {
        intro: intro(tag),
        content: entry_storage_hash(e),
        parent,
        source: e.sym().source.clone(),
        span: e.sym().span.clone(),
    }
}

/// The baseline root: three entries, deliberately supplied out of order so
/// every test exercises the build-time canonicalisation.
fn baseline() -> GenerationRoot {
    let a = entry("alpha", "The alpha module.", "src/lib.rs", 0..10);
    let b = entry("beta", "The beta module.", "src/lib.rs", 20..30);
    let c = entry("gamma", "The gamma module.", "src/other.rs", 5..15);
    GenerationRoot::build(
        lineage(),
        vec![
            row("c", &c, None),
            row("a", &a, None),
            row("b", &b, Some(intro("a"))),
        ],
    )
}

fn intros_of(changes: &[EntryChange]) -> BTreeSet<IntroId> {
    changes.iter().map(EntryChange::intro).collect()
}

// ---------------------------------------------------------------------------
// 1. Canonical encoding — the hash is only an identity if the bytes are
// ---------------------------------------------------------------------------

/// Encoding the same root twice must produce identical bytes. Without this the
/// root hash is not stable and nothing downstream can dedup or compare.
#[test]
fn encoding_is_deterministic() {
    let root = baseline();
    assert_eq!(root.encode(), root.encode());
}

/// THE canonicity test. The same logical set of entries, supplied to `build` in
/// two different orders, must produce byte-identical encodings — otherwise two
/// producers that walked a package in different orders would publish two
/// different hashes for identical IR, and every reuse check would miss.
#[test]
fn build_canonicalises_entry_order() {
    let a = entry("alpha", "The alpha module.", "src/lib.rs", 0..10);
    let b = entry("beta", "The beta module.", "src/lib.rs", 20..30);
    let c = entry("gamma", "The gamma module.", "src/other.rs", 5..15);

    let forward = GenerationRoot::build(
        lineage(),
        vec![row("a", &a, None), row("b", &b, None), row("c", &c, None)],
    );
    let shuffled = GenerationRoot::build(
        lineage(),
        vec![row("c", &c, None), row("a", &a, None), row("b", &b, None)],
    );

    assert_eq!(
        forward.encode(),
        shuffled.encode(),
        "input order must not survive into the encoding"
    );
    assert_eq!(forward.hash(), shuffled.hash());
}

/// Decode must invert encode exactly. A lossy round-trip here is the same class
/// of defect as the wire-format-as-storage-format bug that erased every return
/// type (task #18) — and it would be discovered the same way: on restart.
#[test]
fn encode_decode_round_trips_exactly() {
    let root = baseline();
    let decoded = GenerationRoot::decode(&root.encode()).expect("valid encoding must decode");
    assert_eq!(decoded, root);
    assert_eq!(decoded.hash(), root.hash());
}

/// Every field must survive the round trip, checked individually rather than
/// through a whole-struct compare that a `Default`-heavy decode could satisfy
/// vacuously. Location and parent are exactly what the root exists to carry, so
/// they are what a lossy decode would most plausibly drop.
#[test]
fn the_round_trip_preserves_location_and_parent() {
    let root = baseline();
    let decoded = GenerationRoot::decode(&root.encode()).expect("valid encoding must decode");

    let b = decoded
        .entry(intro("b"))
        .expect("entry `b` must survive decoding");
    assert_eq!(b.parent, Some(intro("a")), "tree edge must survive");
    assert_eq!(b.source, PathBuf::from("src/lib.rs"), "file must survive");
    assert_eq!(b.span, 20..30, "byte span must survive");
}

// ---------------------------------------------------------------------------
// 2. Canonicity enforced on the way *in*
// ---------------------------------------------------------------------------

/// A decoder that accepted unsorted entries would let two distinct byte strings
/// represent one logical root, so the root hash would stop being an identity.
/// Rejecting on decode is what makes the encoding bijective.
#[test]
fn decode_rejects_unsorted_entries() {
    let root = baseline();
    let mut scrambled = root.clone();
    scrambled.entries.reverse();

    // Bypass `build` (which would re-sort) by encoding the scrambled value
    // directly through the same field layout.
    let bytes = scrambled.encode_unchecked();
    assert!(
        GenerationRoot::decode(&bytes).is_err(),
        "a non-canonical encoding must be rejected, not silently accepted and \
         re-sorted — accepting it means two byte strings share one hash preimage"
    );
}

/// An `IntroId` is a *stable identity*: two rows claiming the same one make
/// "which content is entry X?" ambiguous, and would make `diff` produce
/// nonsense. Reject at the boundary.
#[test]
fn decode_rejects_duplicate_intro_ids() {
    let a = entry("alpha", "The alpha module.", "src/lib.rs", 0..10);
    let b = entry("beta", "The beta module.", "src/lib.rs", 20..30);

    let mut doubled = GenerationRoot::build(lineage(), vec![row("a", &a, None)]);
    // Two rows, same identity, different content.
    doubled.entries.push(RootEntry {
        intro: intro("a"),
        ..row("a", &b, None)
    });

    let bytes = doubled.encode_unchecked();
    assert!(
        GenerationRoot::decode(&bytes).is_err(),
        "a duplicate IntroId must be rejected"
    );
}

/// Truncation must be an error, never a short-but-plausible root. This is the
/// storage analogue of the decoder rule `decode_frames` already enforces on the
/// wire: a stream that stops early is truncation, not a small success.
#[test]
fn decode_rejects_a_truncated_encoding() {
    let bytes = baseline().encode();
    let truncated = &bytes[..bytes.len() / 2];
    assert!(
        GenerationRoot::decode(truncated).is_err(),
        "a truncated root must fail to decode rather than yield a partial package"
    );
}

// ---------------------------------------------------------------------------
// 3. The root hash tracks the generation
// ---------------------------------------------------------------------------

/// A content change must change the root hash — this is what makes "has this
/// package changed" a single comparison.
#[test]
fn the_root_hash_changes_when_an_entry_changes() {
    let before = baseline();

    let a = entry("alpha", "Rewritten documentation.", "src/lib.rs", 0..10);
    let b = entry("beta", "The beta module.", "src/lib.rs", 20..30);
    let c = entry("gamma", "The gamma module.", "src/other.rs", 5..15);
    let after = GenerationRoot::build(
        lineage(),
        vec![
            row("a", &a, None),
            row("b", &b, Some(intro("a"))),
            row("c", &c, None),
        ],
    );

    assert_ne!(before.hash(), after.hash());
}

/// A *move* must also change the root hash — the root carries location, so a
/// generation in which code moved is genuinely a different generation. This is
/// not in tension with `moved != changed`: the root changes, while the entry
/// **payloads** it names do not, which is exactly the split that makes a move
/// cost one small root rewrite instead of a full re-store.
#[test]
fn the_root_hash_changes_when_an_entry_only_moves() {
    let before = baseline();

    let a = entry("alpha", "The alpha module.", "src/lib.rs", 0..10);
    let b = entry("beta", "The beta module.", "src/lib.rs", 900..910);
    let c = entry("gamma", "The gamma module.", "src/other.rs", 5..15);
    let after = GenerationRoot::build(
        lineage(),
        vec![
            row("a", &a, None),
            row("b", &b, Some(intro("a"))),
            row("c", &c, None),
        ],
    );

    assert_ne!(
        before.hash(),
        after.hash(),
        "the root records where entries are, so a move is a new generation"
    );
}

// ---------------------------------------------------------------------------
// 4. Diff — the measured heart of the plan
// ---------------------------------------------------------------------------

/// A root against itself has no changes. The degenerate case, and the one a
/// buggy linear merge is most likely to get wrong by emitting spurious pairs.
#[test]
fn diffing_a_root_against_itself_is_empty() {
    let root = baseline();
    let diff = GenerationRoot::diff(&root, &root);
    assert!(
        diff.is_empty(),
        "an unchanged generation must produce no work, got {diff:?}"
    );
}

/// THE test this whole file exists for. An entry that moved — same content,
/// different span — must be classified `Moved`, never `Changed`. Classifying it
/// `Changed` would refetch and re-store a byte-identical payload and reproduce
/// the 58× churn inflation task #17 measured and removed.
#[test]
fn a_move_is_classified_as_moved_not_changed() {
    let unchanged = entry("beta", "The beta module.", "src/lib.rs", 20..30);
    let relocated = entry("beta", "The beta module.", "src/lib.rs", 900..910);

    // Precondition: the payloads really are identical, so a `Changed`
    // classification could only come from the diff looking at location.
    assert_eq!(
        entry_storage_hash(&unchanged),
        entry_storage_hash(&relocated),
        "fixture precondition — storage hash is position-independent"
    );

    let before = GenerationRoot::build(lineage(), vec![row("b", &unchanged, None)]);
    let after = GenerationRoot::build(lineage(), vec![row("b", &relocated, None)]);

    let diff = GenerationRoot::diff(&before, &after);

    assert_eq!(
        diff.changed().len(),
        0,
        "a byte-identical payload must never be reported as changed"
    );
    assert_eq!(
        intros_of(diff.moved()),
        BTreeSet::from([intro("b")]),
        "the relocation must be reported, as a move"
    );
}

/// Reparenting is a move for the same reason: `entry_storage_hash` excludes the
/// `Node`'s tree edges, so the payload is unchanged and only the root's row
/// differs.
#[test]
fn a_reparent_is_classified_as_moved_not_changed() {
    let b = entry("beta", "The beta module.", "src/lib.rs", 20..30);

    let before = GenerationRoot::build(lineage(), vec![row("b", &b, None)]);
    let after = GenerationRoot::build(lineage(), vec![row("b", &b, Some(intro("a")))]);

    let diff = GenerationRoot::diff(&before, &after);
    assert_eq!(diff.changed().len(), 0);
    assert_eq!(intros_of(diff.moved()), BTreeSet::from([intro("b")]));
}

/// All four classifications in one generation, so a diff that collapses two of
/// them cannot pass by getting each in isolation right.
#[test]
fn diff_reports_added_removed_changed_and_moved_together() {
    let a = entry("alpha", "The alpha module.", "src/lib.rs", 0..10);
    let b = entry("beta", "The beta module.", "src/lib.rs", 20..30);
    let c = entry("gamma", "The gamma module.", "src/other.rs", 5..15);

    let before = GenerationRoot::build(
        lineage(),
        vec![row("a", &a, None), row("b", &b, None), row("c", &c, None)],
    );

    // a: untouched. b: moved. c: content changed. d: added. (and `a` stays)
    let b_moved = entry("beta", "The beta module.", "src/lib.rs", 700..710);
    let c_edited = entry("gamma", "Gamma, rewritten.", "src/other.rs", 5..15);
    let d = entry("delta", "The delta module.", "src/new.rs", 0..12);

    let after = GenerationRoot::build(
        lineage(),
        vec![
            row("a", &a, None),
            row("b", &b_moved, None),
            row("c", &c_edited, None),
            row("d", &d, None),
        ],
    );

    let diff = GenerationRoot::diff(&before, &after);

    assert_eq!(intros_of(diff.added()), BTreeSet::from([intro("d")]));
    assert_eq!(intros_of(diff.changed()), BTreeSet::from([intro("c")]));
    assert_eq!(intros_of(diff.moved()), BTreeSet::from([intro("b")]));
    assert!(
        diff.removed().is_empty(),
        "nothing was removed in this generation"
    );
}

/// Removal must be reported, or a stale entry lingers in every downstream view
/// forever and GC (P8) has no way to learn the entry became unreachable.
#[test]
fn diff_reports_a_removed_entry() {
    let a = entry("alpha", "The alpha module.", "src/lib.rs", 0..10);
    let b = entry("beta", "The beta module.", "src/lib.rs", 20..30);

    let before = GenerationRoot::build(lineage(), vec![row("a", &a, None), row("b", &b, None)]);
    let after = GenerationRoot::build(lineage(), vec![row("a", &a, None)]);

    let diff = GenerationRoot::diff(&before, &after);
    assert_eq!(intros_of(diff.removed()), BTreeSet::from([intro("b")]));
    assert!(diff.added().is_empty());
    assert!(diff.changed().is_empty());
}

/// An entry present in neither root must never appear, and an entry present in
/// both and identical must never appear. The plan's whole economy is that the
/// unchanged remainder costs nothing — a diff that emitted every entry would be
/// correct-looking and worthless.
#[test]
fn diff_says_nothing_about_unchanged_entries() {
    let root = baseline();
    let diff = GenerationRoot::diff(&root, &root);
    assert_eq!(diff.added().len(), 0);
    assert_eq!(diff.removed().len(), 0);
    assert_eq!(diff.changed().len(), 0);
    assert_eq!(diff.moved().len(), 0);
}

// ---------------------------------------------------------------------------
// 5. Fault-in manifest
// ---------------------------------------------------------------------------

/// `missing` is the fault-in path (P6): given what the local store already
/// holds, which payloads must actually be fetched? Entries already held must
/// not be requested.
#[test]
fn missing_asks_only_for_payloads_not_already_held() {
    let a = entry("alpha", "The alpha module.", "src/lib.rs", 0..10);
    let b = entry("beta", "The beta module.", "src/lib.rs", 20..30);
    let root = GenerationRoot::build(lineage(), vec![row("a", &a, None), row("b", &b, None)]);

    let held = entry_storage_hash(&a);
    let missing = root.missing(|hash| *hash == held);

    assert_eq!(
        missing,
        vec![entry_storage_hash(&b)],
        "only the payload the caller lacks may be requested"
    );
}

/// THE dedup test. Two declarations with identical content share one storage
/// hash — that sharing is the entire reason the payloads are content-addressed.
/// A fault-in manifest that requested it twice would pay for the duplication it
/// exists to eliminate.
#[test]
fn missing_deduplicates_shared_payloads() {
    // Same content, two identities, two locations.
    let twin_a = entry("twin", "Identical body.", "src/a.rs", 0..10);
    let twin_b = entry("twin", "Identical body.", "src/b.rs", 40..50);
    assert_eq!(
        entry_storage_hash(&twin_a),
        entry_storage_hash(&twin_b),
        "fixture precondition — identical content dedups"
    );

    let root = GenerationRoot::build(
        lineage(),
        vec![row("x", &twin_a, None), row("y", &twin_b, None)],
    );

    let missing = root.missing(|_| false);
    assert_eq!(
        missing.len(),
        1,
        "a shared payload must be requested once, got {missing:?}"
    );
}

/// A store that already has everything must produce no work at all — the
/// warm-cache case, and the one that decides whether a second launch recompiles
/// the world.
#[test]
fn missing_is_empty_when_everything_is_held() {
    let root = baseline();
    assert!(
        root.missing(|_| true).is_empty(),
        "a fully-warm store must fetch nothing"
    );
}
