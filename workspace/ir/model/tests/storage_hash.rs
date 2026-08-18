//! Red-first specification: **a content hash fit to be a storage identity**
//! (`docs/IR-STORAGE-PLAN.md` P2 prerequisite, §3a.1).
//!
//! # The measured defect this closes
//!
//! `entry_content_hash` encodes `sym.source` and `sym.span.start`/`.end`
//! (`src/content/mod.rs:366-370`) — the declaration's **byte offsets in its
//! file**. Editing anything earlier in a file shifts every later declaration's
//! span, so the hash changes for byte-identical code.
//!
//! Measured on one real adjacent-version pair (testify v1.9.0 → v1.11.1):
//!
//! | hash | reported "modified" |
//! |---|---|
//! | `entry_content_hash` as-is | **75.6%** |
//! | same, with `source`/`span` excluded | **1.3%** |
//!
//! A 58× inflation of apparent churn, entirely from code *moving* rather than
//! *changing*. `src/manifest/mod.rs:384-386` already builds prototype
//! `(IntroId, ContentBlake3)` pairs with this hash — exactly the shape
//! `GenerationRoot` wants — so shipping it unchanged would make a real
//! deployment look catastrophic for reasons having nothing to do with real
//! edits.
//!
//! # This is not a bug in `entry_content_hash`
//!
//! Including position is defensible for the question that hash already answers:
//! *"did anything about this entry change, including where it is?"* — its own
//! comment at `:852` says "the source changed", which is true. It is simply the
//! wrong hash for a **storage identity**. Two questions, two hashes. That split
//! is already precedent here: `index::blob::BlobManifest` deliberately keeps
//! Hash① (change detection) distinct from Hash② (storage address), and its doc
//! comment says unifying them is a bug class to avoid.
//!
//! So this spec pins a *second* hash. `entry_content_hash` keeps its meaning and
//! its callers untouched.
//!
//! # The subtlety: where location goes instead
//!
//! Excluding `source`/`span` from the hash is not enough on its own — a
//! consumer still needs to know where a symbol *is*, and two entries whose only
//! difference is position must not collapse into one stored payload with one
//! arbitrary location attached.
//!
//! The resolution is that location is **generation-scoped, not content-scoped**:
//! it belongs in the `GenerationRoot` (which is rewritten every generation
//! anyway, so volatile data costs nothing there) rather than inside the
//! content-addressed body (which is precisely what must stay stable). The
//! entry payload dedups; the root carries where each entry was that time.
//!
//! **Do not weaken these tests to make them pass.**

use std::path::PathBuf;

use nudox_ir::content::{entry_content_hash, entry_storage_hash};
use nudox_ir::entry::{Entry, Node, Symbol, Visibility};
use nudox_ir::index::RawRef;
use nudox_ir::kind::Kind;
use nudox_ir::kinds::Module;

// ---------------------------------------------------------------------------
// helpers
//
// `nudox_ir::test_helpers` is `mod test_helpers;` — private to the crate,
// unreachable from an integration test. These fixtures are instead built from
// the crate's real *public* constructors:
//
// - `Symbol` has entirely `pub` fields (see `nudox_ir::entry::Symbol` and the
//   struct-literal example in the crate's own top-level doc comment), so it is
//   built with a plain struct literal.
// - `Entry::new(sym, node, kind)` is the public owned-entry constructor
//   (`nudox_ir::entry::Entry::new`).
// - `Node::build(parent, children)` builds the structural-edge half of an
//   `Entry`; a root node with no parent/children is
//   `Node::build(None::<RawRef>, [])`.
// - `Kind::Module(Module)` is the smallest available `Kind` payload (a unit
//   struct) — content-hash-irrelevant here since every fixture below shares
//   the same kind body and only `Symbol` fields vary.
//
// Shape is fixed and load-bearing: `at_position` varies ONLY `source`/`span`;
// each `with_*` varies ONLY the named field from `at_position("src/lib.rs", 0,
// 10)`. Do not change what is held constant.
// ---------------------------------------------------------------------------

/// The one non-varying `Symbol`, with `source`/`span` supplied by the caller.
/// Every `with_*` fixture starts from this same set of constants and changes
/// exactly one field, so a test failure can only be attributed to that field.
fn base_symbol(source: &str, start: usize, end: usize) -> Symbol {
    Symbol {
        name: "widget".to_owned(),
        visibility: Visibility::Public,
        documentation: "The base fixture symbol.".to_owned(),
        source: PathBuf::from(source),
        span: start..end,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    }
}

/// Wrap a `Symbol` into a minimal owned `Entry` (root node, `Kind::Module`
/// payload — the payload is irrelevant to every test in this file, which only
/// ever varies `Symbol` fields).
fn entry_for(sym: Symbol) -> Entry {
    Entry::new(sym, Node::build(None::<RawRef>, []), Kind::Module(Module))
}

/// The same declaration, parameterised only by where it sits in its file.
fn at_position(source: &str, start: usize, end: usize) -> Entry {
    entry_for(base_symbol(source, start, end))
}

/// `at_position("src/lib.rs", 0, 10)`, with exactly one semantic field changed.
fn with_name(name: &str) -> Entry {
    let mut sym = base_symbol("src/lib.rs", 0, 10);
    name.clone_into(&mut sym.name);
    entry_for(sym)
}

fn with_documentation(docs: &str) -> Entry {
    let mut sym = base_symbol("src/lib.rs", 0, 10);
    docs.clone_into(&mut sym.documentation);
    entry_for(sym)
}

fn with_visibility(visibility: Visibility) -> Entry {
    let mut sym = base_symbol("src/lib.rs", 0, 10);
    sym.visibility = visibility;
    entry_for(sym)
}

/// A declaration whose position fields are empty/zero, so the position-sensitive
/// encoder contributes nothing distinguishing — isolating the domain separator.
fn at_zero_position() -> Entry {
    entry_for(base_symbol("", 0, 0))
}

// ---------------------------------------------------------------------------
// 1. The defect, pinned
// ---------------------------------------------------------------------------

/// The position-sensitive hash is *documented* to move with position. Pinning it
/// here means the storage-hash tests below cannot pass vacuously by both hashes
/// happening to be position-blind.
#[test]
fn the_existing_content_hash_is_position_sensitive_by_design() {
    let early = at_position("src/lib.rs", 100, 200);
    let moved = at_position("src/lib.rs", 900, 1000);
    assert_ne!(
        entry_content_hash(&early),
        entry_content_hash(&moved),
        "entry_content_hash is expected to track position — if this ever stops \
         being true, the storage hash below is redundant and should be removed"
    );
}

// ---------------------------------------------------------------------------
// 2. The storage hash
// ---------------------------------------------------------------------------

/// THE test. Byte-identical code that merely moved must hash identically, or a
/// one-line edit at the top of a file re-stores every declaration below it.
#[test]
fn the_storage_hash_ignores_where_a_declaration_sits() {
    let early = at_position("src/lib.rs", 100, 200);
    let moved = at_position("src/lib.rs", 900, 1000);
    assert_eq!(
        entry_storage_hash(&early),
        entry_storage_hash(&moved),
        "a declaration that only moved must not be re-stored"
    );
}

/// Moving a declaration to a different file is the same case — the content did
/// not change, so the stored payload must not.
#[test]
fn the_storage_hash_ignores_which_file_a_declaration_is_in() {
    let here = at_position("src/lib.rs", 100, 200);
    let there = at_position("src/moved/elsewhere.rs", 100, 200);
    assert_eq!(
        entry_storage_hash(&here),
        entry_storage_hash(&there),
        "relocating a file must not re-store every declaration in it"
    );
}

// ---------------------------------------------------------------------------
// 3. …but stays sensitive to everything that is actually content
// ---------------------------------------------------------------------------

/// The failure mode on the other side: a hash so blind it merges distinct
/// declarations. Each of these must change the storage hash.
#[test]
fn the_storage_hash_is_sensitive_to_real_content() {
    let base_hash = entry_storage_hash(&at_position("src/lib.rs", 0, 10));

    let renamed = with_name("beta");
    assert_ne!(
        entry_storage_hash(&renamed),
        base_hash,
        "a rename is a content change"
    );

    let redocumented = with_documentation("Different documentation entirely.");
    assert_ne!(
        entry_storage_hash(&redocumented),
        base_hash,
        "documentation is served to readers, so it is content"
    );

    let hidden = with_visibility(Visibility::Private);
    assert_ne!(
        entry_storage_hash(&hidden),
        base_hash,
        "visibility changes what the symbol *is*"
    );
}

/// Determinism: the same entry must hash the same every time, or nothing
/// downstream can dedup at all.
#[test]
fn the_storage_hash_is_deterministic() {
    let entry = at_position("src/lib.rs", 42, 99);
    assert_eq!(
        entry_storage_hash(&entry),
        entry_storage_hash(&entry)
    );
}

/// The two hashes must be domain-separated. If they shared a domain, a storage
/// hash could collide with a change-detection hash of some other entry and one
/// would silently satisfy a lookup meant for the other.
#[test]
fn the_two_hashes_are_domain_separated() {
    let entry = at_zero_position();

    assert_ne!(
        entry_content_hash(&entry).to_string(),
        entry_storage_hash(&entry).to_string(),
        "the two hashes must never coincide, even when their inputs do"
    );
}
