//! **Golden / change-detector tests** for the two deliberately distinct hashes
//! that live on [`BlobManifest`].
//!
//! ## Why these tests exist
//!
//! `BlobManifest` is the centre of two separate hash computations, each with a
//! different semantic role (see the two-hash doc comment in `blob/mod.rs`):
//!
//! - **Hash ①  generation stamp** — `BlobManifest::identity_bytes` fed to
//!   `ContentHash::of_bytes`.  Covers the logical content of the manifest
//!   (sorted file paths + per-file hashes, ir_ref, references_ref, toolchain)
//!   using a hand-rolled, length-prefixed, bespoke encoding.
//!
//! - **Hash ②  serialized-blob CAS key** — `BlobManifest::manifest_cas_key`.
//!   Covers the full postcard wire form of the manifest struct.
//!
//! If either encoding ever changes, the corresponding assertion below will fail
//! **before the change ships**, giving reviewers an explicit decision point
//! rather than a silent cache-invalidation event.
//!
//! ## Fixture
//!
//! All three tests share a single deterministic `BlobManifest` built from known
//! constants.  No wall-clock time, no random ids, no environment state — the
//! fixture is fully reproducible.

mod common;

use heart::ContentHash;
use registry::BlobManifest;

/// Build the fixed deterministic fixture manifest used by all three pin tests.
///
/// Every field is derived from hard-coded constants so the fixture is
/// bit-for-bit reproducible across runs and machines.
fn fixture_manifest() -> BlobManifest {
    let (manifest, _sections) = common::built_manifest(&common::rust_package("serde", "1.0.0"));
    manifest
}

// ─────────────────────────────────────────────────────────────────────────────
// Pin ① — generation-stamp encoding
// ─────────────────────────────────────────────────────────────────────────────

/// The BLAKE3 hash of `identity_bytes()` over the fixture manifest must not
/// change silently.  If this assertion fails, the generation-stamp encoding has
/// changed, which invalidates all previously-stored freshness records.
#[test]
fn identity_bytes_hash_is_stable() {
    let manifest = fixture_manifest();
    let hash = ContentHash::of_bytes(&manifest.identity_bytes());
    assert_eq!(
        hash.hex(),
        // GOLDEN — computed once from the fixture above; update only after an
        // intentional, reviewed encoding change (and bump the generation store).
        "940fa8fa4d6cad9dd91b652ee9d349c578b2f7e077339f41495edd751a82dc3e",
        "generation-stamp encoding changed — see blob/mod.rs Hash ① comment"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Pin ② — serialized-blob CAS key encoding
// ─────────────────────────────────────────────────────────────────────────────

/// The postcard-derived CAS key over the fixture manifest must not change
/// silently.  If this assertion fails, the manifest's wire encoding has
/// changed, which means stored blobs under the old key are unreachable (broken
/// pointer).
#[test]
fn manifest_cas_key_is_stable() {
    let manifest = fixture_manifest();
    let key = manifest.manifest_cas_key().expect("fixture manifest serializes infallibly");
    assert_eq!(
        key.hex(),
        // GOLDEN — computed once from the fixture above; update only after an
        // intentional, reviewed schema change (and bump the manifest codec
        // version / migration path).
        "41ff362a2cf777890d4fc908700e8f1574cafdfe32fd6e1bb87fb1222c732abe",
        "manifest CAS-key encoding changed — see blob/mod.rs Hash ② comment"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Distinctness — documents intent in code
// ─────────────────────────────────────────────────────────────────────────────

/// For the same manifest, Hash ① and Hash ② must be different values.
///
/// This is an invariant, not an accident of the fixture: the two encodings have
/// different inputs (bespoke logical encoding vs full postcard wire form), so
/// equal hashes would mean a collision.  This test ensures a future refactor
/// cannot accidentally unify the two paths.
#[test]
fn generation_stamp_and_cas_key_are_different() {
    let manifest = fixture_manifest();
    let stamp = ContentHash::of_bytes(&manifest.identity_bytes());
    let cas_key = manifest.manifest_cas_key().expect("fixture manifest serializes infallibly");
    assert_ne!(
        stamp, cas_key,
        "Hash ① (generation stamp) and Hash ② (CAS key) must remain distinct for the same manifest"
    );
}
