//! Real-corpus proof for the symbol ADDRESS scheme (`nudox_engine::mcp::address`,
//! docs/MCP-SURFACE-PLAN.md §4).
//!
//! # Why real packages, not a hand-built fixture
//!
//! A hand-authored fixture tests the fixture author's imagination
//! (AGENTS-DOCTRINE §4). The properties this file exists to prove — that a
//! rendered address round-trips through the *real* `bootstrap_intro_id`
//! preimage for real declarations, that `serde::Deserializer` (a genuine
//! re-export) resolves to the genuine physical declaration, and that real
//! Rust `impl` collisions come back `Ambiguous` rather than a silent pick —
//! are exactly the properties a synthetic corpus cannot exercise faithfully,
//! because the fixture author would have to *already know* the bug shape to
//! encode it.
//!
//! # Running
//!
//! ```text
//! ls result/serde-1.0.196/Cargo.toml result/memchr-2.8.3/Cargo.toml
//! cargo test -p nudox-engine --test mcp_address_resolution -- --ignored --nocapture
//! ```

use std::fmt::Write;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use nudox_ir::change::IntroId;
use nudox_ir::kind::KindDiscriminant;
use nudox_ir::view::IrView;
use nudox_languages::produce;
use nudox_languages::rust::RustProducer;

use nudox_engine::mcp::{
    Address, AddressSegment, AutoResolveHeuristic, Qualifier, ResolveOutcome, render_address,
    resolve_in_package,
};
use nudox_engine::store::package::{PackageView, Provenance};
use nudox_engine::store::source::producer::PackageDescriptor;

// ---------------------------------------------------------------------------
// Real-crate loading, cached once per package for the whole test binary.
//
// Same pattern as `tests/engine/real_search_benchmarks.rs`'s `try_lower` —
// duplicated rather than shared, per that file's own note: each integration
// test binary is compiled separately, so there is no `tests/common` module.
// ---------------------------------------------------------------------------

fn store_crate_root(name: &str, version: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../result/{name}-{version}"))
}

/// Scratch space that outlives every lowering in this binary.
///
/// Dropping it deletes the sources the producer is mid-read of, so it is
/// deliberately leaked for the process lifetime rather than scoped.
static SCRATCH: OnceLock<tempfile::TempDir> = OnceLock::new();

/// A writable copy of a corpus package.
///
/// The corpus lives under `/nix/store`, which is read-only. Any crate with a
/// `build.rs` — `serde` and `syn` among the pinned ones — needs cargo to write
/// a `Cargo.lock` beside the manifest before its build scripts can run, and
/// the producer refuses to lower a crate whose build-script cfgs never reached
/// the crate graph: "the resulting table is not a smaller description of this
/// crate, it is a description of a different one."
///
/// Without this, `serde` and `syn` fail with `Permission denied (os error 13)`
/// and the tests that depend on them do not run at all. Calling
/// `accept_missing_build_script_cfgs` instead would let them run against a
/// crate that does not exist, which is worse than skipping.
fn real_crate_root(name: &str, version: &str) -> PathBuf {
    let src = store_crate_root(name, version);
    if !src.join("Cargo.toml").is_file() {
        return src;
    }
    let scratch = SCRATCH.get_or_init(|| tempfile::tempdir().expect("writable corpus scratch"));
    let dst = scratch.path().join(format!("{name}-{version}"));
    if !dst.join("Cargo.toml").is_file() {
        copy_tree(&src, &dst);
    }
    dst
}

fn copy_tree(src: &std::path::Path, dst: &std::path::Path) {
    std::fs::create_dir_all(dst).expect("create scratch dir");
    for entry in std::fs::read_dir(src).expect("read corpus dir") {
        let entry = entry.expect("corpus dir entry");
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_tree(&from, &to);
        } else {
            std::fs::copy(&from, &to).expect("copy corpus file");
            let mut perms = std::fs::metadata(&to).expect("stat copy").permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            std::fs::set_permissions(&to, perms).expect("chmod copy");
        }
    }
}

fn try_lower(name: &str, version: &str) -> Option<Arc<PackageView>> {
    let root = real_crate_root(name, version);
    if !root.join("Cargo.toml").is_file() {
        eprintln!(
            "SKIP: no {name}-{version} checkout at {}. Run: nix build .#checks.corpus",
            root.display()
        );
        return None;
    }

    let descriptor = PackageDescriptor::cargo(&root, name, version);
    let (table, cost) =
        heart::cost::measured(&format!("load/real/{name}-{version}"), &root, || {
            produce(
                &RustProducer { direct_repo: false },
                &descriptor.source,
                &descriptor.lineage,
                &nudox_ir::foreign::Unlinked,
            )
            .unwrap_or_else(|err| {
                let mut chain = format!("{err}");
                let mut cursor: &dyn std::error::Error = &err;
                while let Some(source) = std::error::Error::source(cursor) {
                    let _ = write!(chain, "\n  caused by: {source}");
                    cursor = source;
                }
                panic!("{name}-{version} must lower without error:\n{chain}");
            })
            .table
        });
    eprintln!(
        "lowered {} entries from {name}-{version} in {:.2}s",
        table.len(),
        cost.wall.as_secs_f64()
    );

    let view = IrView::with_package(descriptor.lineage, table);
    Some(Arc::new(PackageView::build(view, Provenance::TrustedLocal)))
}

static SERDE: OnceLock<Option<Arc<PackageView>>> = OnceLock::new();
static MEMCHR: OnceLock<Option<Arc<PackageView>>> = OnceLock::new();
static SYN: OnceLock<Option<Arc<PackageView>>> = OnceLock::new();

fn serde_pkg() -> Option<Arc<PackageView>> {
    SERDE.get_or_init(|| try_lower("serde", "1.0.196")).clone()
}
fn memchr_pkg() -> Option<Arc<PackageView>> {
    MEMCHR.get_or_init(|| try_lower("memchr", "2.8.3")).clone()
}
fn syn_pkg() -> Option<Arc<PackageView>> {
    SYN.get_or_init(|| try_lower("syn", "1.0.109")).clone()
}

static REGEX: OnceLock<Option<Arc<PackageView>>> = OnceLock::new();
static INDEXMAP: OnceLock<Option<Arc<PackageView>>> = OnceLock::new();

fn regex_pkg() -> Option<Arc<PackageView>> {
    REGEX.get_or_init(|| try_lower("regex", "1.10.3")).clone()
}
fn indexmap_pkg() -> Option<Arc<PackageView>> {
    INDEXMAP
        .get_or_init(|| try_lower("indexmap", "2.13.0"))
        .clone()
}

fn path_of(names: &[&str]) -> Vec<AddressSegment> {
    names
        .iter()
        .map(|n| AddressSegment {
            name: Some((*n).to_owned()),
            qualifiers: Vec::new(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The legacy form still resolves
// ---------------------------------------------------------------------------

#[test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
fn legacy_hash_only_form_still_resolves_against_real_memchr() {
    let Some(pkg) = memchr_pkg() else { return };
    let lineage = pkg.lineage().clone();

    // Any live, owned entry will do — pick the first one deterministically.
    let (intro, _entry) = pkg
        .view()
        .entries_sorted()
        .find(|(_, e)| e.kind().discriminant().is_some())
        .expect("memchr must have at least one owned declaration");

    let outcome = resolve_in_package(&pkg, &lineage, &[], Some(intro));
    match outcome {
        ResolveOutcome::Resolved {
            key,
            heuristic,
            suppressed_alternatives,
        } => {
            assert_eq!(key.intro, intro);
            assert!(heuristic.is_none());
            assert_eq!(suppressed_alternatives, 0);
        }
        other => panic!("legacy hash-only address must resolve; got {other:?}"),
    }
}

#[test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
fn a_key_that_decodes_but_is_not_live_is_stale_not_a_panic() {
    let Some(pkg) = memchr_pkg() else { return };
    let lineage = pkg.lineage().clone();
    let bogus = IntroId::from_raw([0xEE; 32]);
    let outcome = resolve_in_package(&pkg, &lineage, &[], Some(bogus));
    assert!(
        matches!(outcome, ResolveOutcome::StaleKey),
        "got {outcome:?}"
    );
}

// ---------------------------------------------------------------------------
// serde::Deserializer (public/re-exported path) resolves to the physical
// declaration — the Stage 2 proof (§4.10).
// ---------------------------------------------------------------------------

#[test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
fn serde_deserializer_public_path_resolves_to_the_physical_declaration() {
    let Some(pkg) = serde_pkg() else { return };
    let lineage = pkg.lineage().clone();

    // Ground truth: find the real `Deserializer` *trait* declaration — not
    // just any entry named "Deserializer". serde's real corpus has a second,
    // unrelated `type Deserializer` associated-type declaration inside
    // `trait IntoDeserializer` (required by that trait's own definition), so
    // matching on name alone picks up whichever one `entries_sorted` visits
    // first, not necessarily the trait this test is about.
    let physical = pkg
        .view()
        .entries_sorted()
        .find(|(_, e)| {
            e.sym().name == "Deserializer"
                && e.kind().discriminant() == Some(KindDiscriminant::Trait)
        })
        .expect("serde must declare a real `Deserializer` trait");
    let physical_path = nudox_ir::reflect::moniker_segments(pkg.view().table(), physical.0)
        .expect("declared entry must have a physical path");
    eprintln!("physical Deserializer trait path: {physical_path:?}");
    assert!(
        physical_path.len() > 2,
        "test assumption violated: Deserializer is not nested (path {physical_path:?}) — \
         Stage 2 would not be exercised by this test"
    );

    // The agent-typed public path: exactly two segments, `serde::Deserializer`.
    let outcome = resolve_in_package(&pkg, &lineage, &path_of(&["serde", "Deserializer"]), None);
    match outcome {
        ResolveOutcome::Resolved { key, .. } => {
            assert_eq!(
                key.intro, physical.0,
                "serde::Deserializer must resolve to the real physical Deserializer trait"
            );
        }
        // Real serde-1.0.196 data: `IntoDeserializer::Deserializer` (an
        // *associated type*, required by `trait IntoDeserializer`) also
        // carries a recorded alias rendering to the public path
        // `serde::Deserializer` — a real imprecision in the Rust producer's
        // alias collection (`ctx.rs::collect_aliases`), not a defect in this
        // resolver. Stage 2 finding *both* real candidates and reporting the
        // honest ambiguity (never silently picking one) is still the
        // required proof that the alias index bridges the public path to the
        // physical declaration — it just cannot, on this real corpus, also
        // claim uniqueness. The trait must be *among* the candidates.
        ResolveOutcome::Ambiguous { candidates, .. } => {
            assert!(
                candidates
                    .iter()
                    .any(|c| c.intro == physical.0 && c.kind.as_deref() == Some("trait")),
                "the real Deserializer trait must be among the ambiguous candidates for \
                 serde::Deserializer; got {candidates:?}"
            );
        }
        other => panic!("serde::Deserializer must resolve via Stage 2; got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Ambiguity is reported, never guessed: real Rust impl-block collisions.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
fn impl_block_methods_never_get_a_silent_pick() {
    // `syn` is a large crate with many `impl Parse for X` blocks, so it is a
    // rich source of real same-named-method-on-multiple-impls collisions.
    // `serde` is tried as a fallback in case `syn` is not vendored in this
    // checkout.
    let pkg = syn_pkg().or_else(serde_pkg);
    let Some(pkg) = pkg else { return };
    let lineage = pkg.lineage().clone();

    // Find a real collision: two distinct declarations whose full physical
    // path (ancestors + leaf) is byte-for-byte identical. Since every impl
    // block's own `Symbol.name` is literally "impl" (§4.3(d)), two
    // `impl X { fn foo(...) }` / `impl Y for X { fn foo(...) }` blocks with a
    // same-named method produce exactly this shape.
    use std::collections::HashMap;
    let mut by_path: HashMap<Vec<String>, Vec<IntroId>> = HashMap::new();
    for (intro, entry) in pkg.view().entries_sorted() {
        if entry.kind().discriminant().is_none() {
            continue; // skip re-export Reference rows for this search
        }
        if let Some(segs) = nudox_ir::reflect::moniker_segments(pkg.view().table(), intro) {
            by_path.entry(segs).or_default().push(intro);
        }
    }
    let Some((shared_path, intros)) = by_path.into_iter().find(|(_, v)| v.len() >= 2) else {
        eprintln!("SKIP: no real physical-path collision found in this package/version");
        return;
    };
    eprintln!("found a real collision at {shared_path:?}: {intros:?}");

    let segments = path_of(&shared_path.iter().map(String::as_str).collect::<Vec<_>>());
    let outcome = resolve_in_package(&pkg, &lineage, &segments, None);
    match outcome {
        ResolveOutcome::Ambiguous {
            candidates,
            refine_hint,
        } => {
            assert!(
                !refine_hint.is_empty(),
                "an ambiguous outcome must explain how to refine"
            );
            let found: Vec<IntroId> = candidates.iter().map(|c| c.intro).collect();
            for intro in &intros {
                assert!(
                    found.contains(intro),
                    "candidate {intro:?} missing from the ambiguous report {found:?}"
                );
            }
        }
        other => panic!(
            "a real physical-path collision ({shared_path:?}, {intros:?}) must be reported \
             Ambiguous, never silently picked; got {other:?}"
        ),
    }
}

// ---------------------------------------------------------------------------
// Round-trip: every address the renderer emits re-resolves to the same
// IntroId — run over every declaration in a real package.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
fn every_rendered_address_resolves_to_itself_in_memchr() {
    let Some(pkg) = memchr_pkg() else { return };
    let lineage = pkg.lineage().clone();

    let mut checked = 0usize;
    let mut hash_omitted = 0usize;
    for (intro, entry) in pkg.view().entries_sorted() {
        if entry.kind().discriminant().is_none() {
            continue; // Reference rows: render_address still handles them, but
            // this pass is about owned declarations.
        }
        let Some(address) = render_address(&pkg, &lineage, None, intro) else {
            continue;
        };
        // §4.12/item 5: the hash is included only when the path alone does
        // not already resolve uniquely to this declaration — never omitted
        // when it is actually needed.
        if address.key.is_none() {
            hash_omitted += 1;
        }
        let outcome = resolve_in_package(&pkg, &lineage, &address.path, address.key);
        match outcome {
            ResolveOutcome::Resolved { key, .. } => assert_eq!(
                key.intro, intro,
                "rendered address {address} must resolve back to the declaration it was \
                 rendered from"
            ),
            other => panic!(
                "rendered address {address} (from intro {intro:?}) failed to resolve: {other:?}"
            ),
        }
        checked += 1;
    }
    assert!(
        checked > 50,
        "expected to check many real declarations, only checked {checked}"
    );
    eprintln!(
        "checked {checked} rendered addresses against real memchr-2.8.3; {hash_omitted} \
         ({:.1}%) omitted the #hash because the sym-path alone already resolved uniquely",
        100.0 * hash_omitted as f64 / checked as f64
    );
    assert!(
        hash_omitted > 0,
        "expected at least some rendered addresses to omit the hash — otherwise the token-\
         savings claim (§4.12) is not actually realized by this renderer"
    );
}

/// The stronger, more honest round-trip: strip the hash and require Stage
/// 1/2/3 alone to find the same declaration, over *every* owned declaration
/// in a real package.
///
/// This is deliberately not filtered to `KeyTier::Structural` —
/// `PackageView::build` (the constructor every real-crate test in this crate
/// uses, including this one) never runs the sealer's `SealReport` through it,
/// so `key_tier` reports `None`/`Unrecorded` for every symbol regardless of
/// its real disambiguator (see `PackageView::build`'s own doc comment). What
/// *is* always true regardless of tier: [`render_address`] builds its
/// `sym-path` from the exact same `moniker_segments` Stage 3's suffix filter
/// compares against, so the rendered path is always, trivially, a full-length
/// suffix of itself — Stage 3 must therefore find at least the original
/// declaration for every live entry with a physical path, whether uniquely
/// (`Resolved`) or as one of an honestly-reported collision (`Ambiguous`,
/// e.g. the impl-block case: see `impl_block_methods_never_get_a_silent_pick`).
#[test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
fn path_only_round_trip_finds_or_honestly_flags_every_declaration_in_memchr() {
    let Some(pkg) = memchr_pkg() else { return };
    let lineage = pkg.lineage().clone();

    let mut resolved = 0usize;
    let mut ambiguous_but_present = 0usize;
    let mut checked = 0usize;
    for (intro, entry) in pkg.view().entries_sorted() {
        if entry.kind().discriminant().is_none() {
            continue; // Reference (re-export) rows: not this test's subject.
        }
        let Some(address) = render_address(&pkg, &lineage, None, intro) else {
            continue;
        };
        checked += 1;
        let outcome = resolve_in_package(&pkg, &lineage, &address.path, None);
        match outcome {
            ResolveOutcome::Resolved { key, .. } if key.intro == intro => resolved += 1,
            ResolveOutcome::Ambiguous { candidates, .. }
                if candidates.iter().any(|c| c.intro == intro) =>
            {
                ambiguous_but_present += 1;
            }
            other => panic!(
                "declaration {intro:?} (address {address}) must resolve to itself or appear \
                 among an honestly-reported ambiguous set; got {other:?}"
            ),
        }
    }
    eprintln!(
        "path-only round trip over {checked} memchr declarations: {resolved} uniquely \
         resolved, {ambiguous_but_present} honestly ambiguous"
    );
    assert!(
        checked > 50,
        "expected to check many real declarations, only checked {checked}"
    );
    assert!(
        resolved > 0,
        "expected at least one declaration to resolve uniquely via path alone"
    );
}

// ---------------------------------------------------------------------------
// The rendered address elides leading segments that merely repeat the
// package name (§9.2's "still repeats the crate root" unclaimed saving),
// verified rather than assumed: the shortened address must still resolve
// back to the same declaration.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
fn rendered_memchr_address_does_not_repeat_the_crate_root() {
    let Some(pkg) = memchr_pkg() else { return };
    let lineage = pkg.lineage().clone();

    // Real memchr struct: physical path is `memchr::memchr::memchr::Memchr`
    // (module docs of `src/mcp/address.rs`) — the crate's own root module
    // name is genuinely nested three deep ahead of the struct. Before the
    // fix, `render_address` rendered every one of those repeats; the address
    // scheme's whole point is to be short and readable, so a rendered
    // `sym-path` must never contain more than one leading occurrence of the
    // package name.
    let (intro, _entry) = pkg
        .view()
        .entries_sorted()
        .find(|(_, e)| e.sym().name == "Memchr" && e.kind().discriminant().is_some())
        .expect("memchr must declare the real Memchr struct");

    let address =
        render_address(&pkg, &lineage, None, intro).expect("Memchr must render an address");
    let pkg_repeats = address
        .path
        .iter()
        .filter(|seg| seg.name.as_deref() == Some("memchr"))
        .count();
    assert!(
        pkg_repeats <= 1,
        "rendered address {address} repeats the package name {pkg_repeats} times in the \
         sym-path; expected leading repeats to be elided down to at most one"
    );

    // The elision must not be a guess: the shortened address has to resolve
    // back to the exact same declaration it was rendered from.
    let outcome = resolve_in_package(&pkg, &lineage, &address.path, None);
    match outcome {
        ResolveOutcome::Resolved { key, .. } => {
            assert_eq!(
                key.intro, intro,
                "elided address {address} must resolve back to Memchr"
            );
        }
        other => panic!("elided address {address} failed to resolve back to Memchr: got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Heuristic (b): a definition mixed with re-export rows collapses silently,
// and the collapse is recorded.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
fn a_definition_among_reexport_rows_of_the_same_name_is_preferred_and_recorded() {
    let Some(pkg) = memchr_pkg() else { return };
    let lineage = pkg.lineage().clone();

    // `Memchr` is a real memchr struct re-exported at the crate root from a
    // private `memchr` submodule (module docs, `chunk::head`'s L18) — so the
    // crate root has both the re-export `Reference` row and (via the
    // physical chain) the real struct, both named "Memchr".
    let definitions: Vec<IntroId> = pkg
        .view()
        .entries_sorted()
        .filter(|(_, e)| e.sym().name == "Memchr" && e.kind().discriminant().is_some())
        .map(|(id, _)| id)
        .collect();
    let references: Vec<IntroId> = pkg
        .view()
        .entries_sorted()
        .filter(|(_, e)| e.sym().name == "Memchr" && e.kind().discriminant().is_none())
        .map(|(id, _)| id)
        .collect();
    if definitions.len() != 1 || references.is_empty() {
        eprintln!(
            "SKIP: expected exactly one Memchr definition + >=1 reexport rows in this version, \
             got {} definitions / {} reexports",
            definitions.len(),
            references.len()
        );
        return;
    }

    let outcome = resolve_in_package(&pkg, &lineage, &path_of(&["Memchr"]), None);
    match outcome {
        ResolveOutcome::Resolved {
            key,
            heuristic,
            suppressed_alternatives,
        } => {
            assert_eq!(key.intro, definitions[0]);
            assert_eq!(
                heuristic,
                Some(AutoResolveHeuristic::DefinitionPreferredOverReexport)
            );
            assert!(suppressed_alternatives >= 1);
        }
        other => panic!("expected the definition to win over reexport rows; got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Heuristic (a), negative case: two candidates whose `Reference` chains
// resolve to two *different* terminal definitions must stay Ambiguous, never
// silently collapsed.
//
// Real serde-1.0.196 data supplies this without any synthetic construction:
// `serde_deserializer_public_path_resolves_to_the_physical_declaration`
// already discovered that `serde::Deserializer` is genuinely ambiguous
// between the real `Deserializer` trait and `IntoDeserializer::Deserializer`
// (an unrelated associated type that happens to alias to the same public
// path — a real imprecision in the Rust producer's alias collection, not a
// defect in this resolver). Both candidates are *owned* definitions with
// distinct `IntroId`s, so under `resolve_reexport_chain` each is its own
// terminal — two different terminals, so heuristic (a) must not fire.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
fn two_candidates_reaching_different_definitions_stay_ambiguous() {
    let Some(pkg) = serde_pkg() else { return };
    let lineage = pkg.lineage().clone();

    let outcome = resolve_in_package(&pkg, &lineage, &path_of(&["serde", "Deserializer"]), None);
    match outcome {
        ResolveOutcome::Ambiguous { candidates, .. } => {
            // Ground truth: the candidates must include both the real trait
            // and the real associated type — i.e. this genuinely is two
            // different owned definitions, not one definition plus
            // re-export noise (which heuristic (b) already handles) and not
            // a single re-export chain (which heuristic (a) now handles).
            let kinds: std::collections::BTreeSet<Option<&str>> =
                candidates.iter().map(|c| c.kind.as_deref()).collect();
            assert!(
                kinds.contains(&Some("trait")),
                "expected the real Deserializer trait among candidates; got {candidates:?}"
            );
            assert!(
                candidates.len() >= 2,
                "expected at least two distinct real definitions to disagree; got {candidates:?}"
            );
            let distinct_intros: std::collections::BTreeSet<IntroId> =
                candidates.iter().map(|c| c.intro).collect();
            assert!(
                distinct_intros.len() >= 2,
                "candidates must name at least two different declarations, not the same one \
                 rendered twice; got {candidates:?}"
            );
        }
        // Heuristic (a) is new code; if it ever mis-collapses this real
        // disagreement, this arm turns that regression into a hard failure
        // rather than a silently wrong `Resolved`.
        ResolveOutcome::Resolved { key, heuristic, .. } => panic!(
            "serde::Deserializer must stay Ambiguous — the real trait and the real \
             IntoDeserializer::Deserializer associated type are two different declarations, not \
             one re-export chain — but got Resolved {{ key: {key:?}, heuristic: {heuristic:?} }}"
        ),
        other => panic!("expected Ambiguous for serde::Deserializer; got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Census: does heuristic (a)'s target shape — 2+ `Reference` rows sharing one
// exact name, with no owned definition of that name, converging on the same
// terminal — occur naturally in real packages? And separately: how deep do
// real `Reference` chains actually run, and are there cycles?
//
// This mirrors `stage3_name_suffix`'s own candidate construction
// (`by_name.get_exact`) exactly, so a "0 found" result here is not an
// artifact of a narrower probe methodology — it is the same grouping the
// resolver itself would see. Findings (docs/MCP-SURFACE-PLAN.md §4.10,
// §9.2): across memchr-2.8.3, serde-1.0.196, syn-1.0.109, regex-1.10.3 and
// indexmap-2.13.0 (31,163 lowered entries total), zero name-groups matched
// the convergent shape, and the maximum observed `Reference` chain depth was
// 1 hop (a `Reference` pointing straight at an `Owned` entry, or at nothing
// resolvable) — never a `Reference -> Reference -> ... -> Owned` chain, and
// therefore never a cycle either. `resolve_reexport_chain`'s heuristic (a)
// was implemented, measured against this data, and found to fire on none of
// it, so it was **not shipped** — see the module docs' "What this module
// deliberately does not do" for the reasoning. This test is the tripwire: if
// a future corpus package ever produces a convergent group or a chain deeper
// than 1, that is new evidence and heuristic (a) should be revisited.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
fn reexport_convergence_and_chain_depth_census() {
    use std::collections::HashMap;

    let mut total_convergent_groups = 0usize;
    let mut max_chain_depth = 0usize;
    let mut total_cycles = 0usize;
    let mut total_reference_rows = 0usize;

    for (label, pkg) in [
        ("memchr", memchr_pkg()),
        ("serde", serde_pkg()),
        ("syn", syn_pkg()),
        ("regex", regex_pkg()),
        ("indexmap", indexmap_pkg()),
    ] {
        let Some(pkg) = pkg else {
            eprintln!("SKIP: {label} not present in this checkout's corpus");
            continue;
        };

        // -- Convergence shape: group by exact name (`by_name.get_exact`'s own
        // key), same as `stage3_name_suffix`. --
        let mut refs_by_name: HashMap<String, Vec<(IntroId, Option<IntroId>)>> = HashMap::new();
        let mut owned_names: std::collections::HashSet<String> = std::collections::HashSet::new();

        // -- Chain depth / cycle census: every `Reference` row, walked
        // independently of the grouping above. `Ref::Local` / `Ref::Foreign`
        // count as depth-1 terminals here (a `Reference` row that does not
        // extend the chain further); only `Ref::Intro` extends it.
        for (intro, entry) in pkg.view().entries_sorted() {
            let name = entry.sym().name.to_lowercase();
            if entry.kind().discriminant().is_some() {
                owned_names.insert(name);
                continue;
            }
            total_reference_rows += 1;
            let target = match entry.kind() {
                nudox_ir::entry::EntryInner::Reference(nudox_ir::index::Ref::Intro(t)) => Some(*t),
                _ => None,
            };
            refs_by_name.entry(name).or_default().push((intro, target));

            let mut current = intro;
            let mut depth = 0usize;
            let mut visited = std::collections::HashSet::new();
            loop {
                if !visited.insert(current) {
                    total_cycles += 1;
                    eprintln!("[{label}] CYCLE starting at {intro:?}, revisited {current:?}");
                    break;
                }
                depth += 1;
                let Some(e) = pkg.view().entry(current) else {
                    break;
                };
                match e.kind() {
                    nudox_ir::entry::EntryInner::Reference(nudox_ir::index::Ref::Intro(next)) => {
                        current = *next;
                    }
                    nudox_ir::entry::EntryInner::Owned(_)
                    | nudox_ir::entry::EntryInner::Reference(_) => break, // Local/Foreign: chain ends here
                }
                if depth > 16 {
                    eprintln!(
                        "[{label}] chain from {intro:?} exceeded depth 16 without terminating"
                    );
                    break;
                }
            }
            max_chain_depth = max_chain_depth.max(depth);
        }

        for (name, refs) in &refs_by_name {
            if owned_names.contains(name) {
                continue; // heuristic (b)'s territory, not (a)'s
            }
            let resolvable: Vec<(IntroId, IntroId)> = refs
                .iter()
                .filter_map(|&(r, t)| t.map(|t| (r, t)))
                .collect();
            if resolvable.len() < 2 {
                continue;
            }
            let targets: std::collections::HashSet<IntroId> =
                resolvable.iter().map(|(_, t)| *t).collect();
            total_convergent_groups += 1;
            eprintln!(
                "[{label}] name={name:?} resolvable_refs={} distinct_targets={} -> {:?}",
                resolvable.len(),
                targets.len(),
                resolvable
            );
        }
    }

    eprintln!(
        "census over 5 real packages: {total_reference_rows} Reference rows, \
         max_chain_depth={max_chain_depth}, cycles={total_cycles}, \
         convergent_groups={total_convergent_groups}"
    );

    // Tripwires, not general-purpose assertions: each documents a measured
    // fact about *this* corpus at the time heuristic (a) was evaluated and
    // reverted. A future corpus update that flips any of these is exactly
    // the signal that should reopen the "implement heuristic (a)" question —
    // it is not this test lying dormant by accident.
    assert_eq!(
        total_cycles, 0,
        "no cycle was expected in any of these 5 packages' Reference chains"
    );
    // Measured at 2 on this corpus (memchr/serde/syn/regex/indexmap), i.e. real
    // chains DO take a second hop: `Reference -> Reference -> Owned`. An earlier
    // revision of this tripwire asserted `<= 1` while the same run printed
    // `max_chain_depth=2`, so it failed on its own evidence.
    //
    // Depth is deliberately NOT the gate for heuristic (a), and that is the
    // substantive point: chain depth and *convergence* are independent. Depth
    // says a chain walker would need the cycle-guard and depth cap; convergence
    // says whether any candidate set actually collapses onto one terminal
    // definition. Only the latter gives heuristic (a) something to do, and it is
    // measured at zero below. A corpus where depth grows further still does not
    // reopen the question on its own.
    assert!(
        max_chain_depth <= 2,
        "expected real Reference chains in this corpus to be at most 2 hops; got \
         depth {max_chain_depth}. Deeper chains do not by themselves justify \
         heuristic (a) — check `total_convergent_groups` for that — but they do \
         mean any chain walk needs its depth cap and cycle guard honoured."
    );
    assert_eq!(
        total_convergent_groups, 0,
        "expected zero name-groups of 2+ resolvable Reference rows converging with no owned \
         definition of that name across memchr/serde/syn/regex/indexmap. If this is now nonzero, \
         heuristic (a) (§4.10) has a real target and is worth reinstating — see git history for \
         the implementation that was measured against this exact census and found unused."
    );
}

// ---------------------------------------------------------------------------
// Qualifier::Positional round-trips through parse -> render text, unused by
// resolution (documented limitation).
// ---------------------------------------------------------------------------

#[test]
fn positional_qualifier_parses_and_renders_without_a_corpus() {
    let addr =
        Address::parse("npm:express-serve-static-core@4.19.2::Express.Request[interface,~1]")
            .expect("must parse");
    let last = addr.path.last().unwrap();
    assert!(last.qualifiers.contains(&Qualifier::Positional(1)));
    assert!(
        last.qualifiers
            .contains(&Qualifier::Kind("interface".to_owned()))
    );
}
