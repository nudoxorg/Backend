//! Verify docs/LIMITATIONS.md L19 against the *real* `memchr` crate, end to end
//! through the public renderer.
//!
//! # What this proves
//!
//! `fn memchr(needle: u8, haystack: &[u8]) -> Option<usize>` was documented as
//! rendering with the `Option<..>` wrapper dropped:
//! `fn memchr(needle: u8, haystack: &[u8]) -> usize`. This test lowers real
//! `memchr` through the real producer and renders the real `memchr` function
//! through `nudox_engine::chunk::signature::tokens` — the one renderer every
//! surface uses (LR-4) — and asserts the rendered return type contains
//! `Option`.
//!
//! **Status, measured 2026-08-07: this now PASSES.** Command and output:
//!
//! ```text
//! cargo test -p nudox-engine --test real_memchr_generic_return -- --ignored --nocapture
//!   ra_load std_integration_features_excluded=[core,rustc-dep-of-std] package=memchr
//!   cost case=l19/real/memchr-2.8.3 wall_ms=29832.6 rss_bytes=620068864
//!   lowered 1835 entries from memchr-2.8.3 in 29.8s
//!   test real_memchr_return_type_keeps_option_wrapper ... ok
//! ```
//!
//! Note what the run does *not* print: there is no `metadata_degraded=true`
//! line. Doctrine §8 requires checking that before trusting any number taken
//! here, because upstream `ra_ap_project_model` silently substitutes
//! `--no-deps` metadata on failure and every Cargo feature then evaluates
//! false. This run resolved dependencies properly and excluded the
//! `rustc-std-workspace-*` shims (L38), which is why the count is 1,835 and
//! not the 11,329 that three memchr generations all used to report.
//!
//! # The history this file is the regression guard for
//!
//! Kept because it is the trail back to *why* the failure happened, and
//! because nothing else in the repo records it. Do not delete it just because
//! the test is green.
//!
//! The bug was never in this crate's two owned files (`ty.rs`,
//! `signature.rs`). Direct instrumentation of `ty.rs::lower_path_type` while
//! lowering this exact checkout showed every one of `memchr`'s ~150 uses of
//! `Option<T>` — including its own return type — resolving through
//! `PathResolution::Def` and building
//! `Type::Apply { base: Nominal(foreign_ref), args: [usize] }` correctly.
//! `signature.rs::push_type` renders `Type::Apply` as `base<args>`
//! recursively and did so correctly for the same construct in
//! `generic_signature_shapes.rs` (a fixture immune to the bug).
//!
//! What actually happened: `memchr`'s own `return` `Param` entry was
//! overwritten, post-seal, by one of 18 *sibling* free functions
//! (`memchr2`, `memchr3`, `memrchr`, `memrchr2`, `memrchr3`, and their
//! `_raw`/`_iter` variants) declared in the same module — all of whose
//! `return` Param entries collided onto one `IntroId` because:
//!
//! 1. `workspace/compiler/languages/rust/src/ra/item.rs` declared every
//!    parameter (`declare_params`, and the `output_refs` closure inside
//!    `lower_free_function_with_id`) under the *function's enclosing*
//!    `parent` (its module or impl), not under the function's own id — so a
//!    `return` Param's ancestor-path, as seen by the seal pass, did not
//!    include the owning function's name at all.
//! 2. `workspace/ir/model/src/package/seal.rs`'s collision disambiguator
//!    special-cased only `Kind::Function` and `Kind::Impl`; every other kind
//!    — including `Kind::Param` — fell back to `Disambiguator::Span`, and
//!    every producer-synthesized param symbol carries `span: 0..0`
//!    (`item.rs::plain_sym`), so the "disambiguator" was identical for every
//!    colliding param and resolved nothing.
//!
//! **FIXED 2026-08-08 (L23), at the declaration site named in (1), not the
//! disambiguator named in (2).** `declare_params` and the `output_refs`
//! closure inside `lower_free_function_with_id` now parent every `Param` —
//! inputs and the synthetic `return` output — on `fn_id`, the owning
//! function's own id, instead of on the function's enclosing module/impl
//! (`item.rs:631` and `item.rs:644`, with the shared helper at
//! `item.rs:2200`). That puts the owning function's name into the param's
//! ancestor-path itself, so sibling functions' same-named params are
//! distinct at the `(kind, ancestor-path, leaf-name)` key `seal.rs` uses
//! *before* any disambiguator is chosen — mechanism (2) above is untouched
//! and irrelevant now, because the collision it used to fail to break never
//! forms. Verified by mutation: with `seal.rs`'s escalation pass
//! (`Disambiguator::Span` → `Ordinal`) forced to a no-op, this test still
//! passes — proof the fix removed the collision rather than adding another
//! layer for escalation to rescue. `workspace/compiler/languages/rust/tests/param_identity.rs`
//! carries the stronger, non-real-crate regression guard for the identity
//! property itself: a param's `IntroId` is unchanged by inserting an
//! unrelated sibling function earlier in the same module (escalation cannot
//! promise that — an ordinal is a declaration-order position, not an
//! identity), and two independent lowerings of the same source mint the same
//! `IntroId` for the same param.
//!
//! The escalation pass described above is unchanged and remains a legitimate
//! backstop for *other* collision sources — this fix closes one source of
//! them, not the mechanism itself.
//!
//! # What this test deliberately does NOT check
//!
//! Entry counts. A whole-table count is not this file's invariant, and a
//! count assert here used to run *before* the `Option` check and killed the
//! test on an unrelated arithmetic identity, so the thing it was written to
//! prove never executed. Corpus entry counts have one authoritative home:
//! `nix/entry-baseline.toml`, enforced by `nudox-store`'s
//! `tests/corpus_contract.rs`.
//!
//! # Running
//!
//! ```text
//! cargo test -p nudox-engine --test real_memchr_generic_return -- --ignored --nocapture
//!
//! # Or point it at a different checkout, matching nudox-store's real_package.rs:
//! NUDOX_PKG_ROOT=/path/to/memchr-X.Y.Z NUDOX_PKG_NAME=memchr NUDOX_PKG_VERSION=X.Y.Z \
//!   cargo test -p nudox-engine --test real_memchr_generic_return -- --ignored --nocapture
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use nudox_ir::kind::Kind;
use nudox_ir::view::IrView;
use nudox_producer::produce;
use nudox_producer_rust::RustProducer;
use nudox_store::package::{PackageView, Provenance};
use nudox_store::source::producer::PackageDescriptor;

use nudox_engine::chunk::signature;
use nudox_engine::wire::SigToken;

/// Read an env var, treating exported-but-blank as unset — mirrors
/// `nudox-store/tests/real_package.rs::var`.
fn var(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

fn root() -> PathBuf {
    var("NUDOX_PKG_ROOT").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../result/memchr-2.8.3")
    })
}

fn name() -> String {
    var("NUDOX_PKG_NAME").unwrap_or_else(|| "memchr".to_owned())
}

fn version() -> String {
    var("NUDOX_PKG_VERSION").unwrap_or_else(|| "2.8.3".to_owned())
}

fn render_text(toks: &[SigToken]) -> String {
    let mut out = String::new();
    for tok in toks {
        match tok {
            SigToken::Kw(s) | SigToken::Punct(s) => out.push_str(s),
            SigToken::Ident(s)
            | SigToken::Ty { text: s, .. }
            | SigToken::Generic(s)
            | SigToken::Lifetime(s) => out.push_str(s),
            SigToken::Ws => out.push(' '),
            _ => {}
        }
    }
    out
}

/// `memchr(needle: u8, haystack: &[u8]) -> Option<usize>` must render with
/// `Option` intact.
///
/// Passing as of 2026-08-07 (see the module doc for the measured run and for
/// the param/return identity collision this remains the regression guard for).
#[test]
#[ignore = "drives in-process rust-analyzer over the real result/memchr-2.8.3 cargo \
            workspace: ~30 s and needs the fixture fetched. Run it on purpose with \
            `cargo test -p nudox-engine --test real_memchr_generic_return -- --ignored \
            --nocapture`; it passes"]
fn real_memchr_return_type_keeps_option_wrapper() {
    let root = root();
    assert!(
        root.join("Cargo.toml").is_file(),
        "no crate checkout at {}. Run scripts/fetch-real-crate.sh {} {} \
         or set NUDOX_PKG_ROOT.",
        root.display(),
        name(),
        version()
    );

    let descriptor = PackageDescriptor::cargo(&root, name(), version());
    let (table, cost) = nudox_test_support::measured(
        &format!("l19/real/{}-{}", name(), version()),
        &root,
        || {
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
                    chain.push_str(&format!("\n  caused by: {source}"));
                    cursor = source;
                }
                panic!("{} must lower without error:\n{chain}", name());
            })
        .table},
    );
    eprintln!(
        "lowered {} entries from {}-{} in {:.1}s",
        table.len(),
        name(),
        version(),
        cost.wall.as_secs_f64()
    );

    // NOTE: this test deliberately asserts NOTHING about `table.len()`.
    //
    // A whole-table entry count is not the invariant this file exists to
    // prove, and doctrine §4 is explicit that a test asserts on *content*, not
    // on a count. A count assert here was also actively harmful: it ran before
    // the real assertion below, so the test died on an unrelated arithmetic
    // identity and the `Option<..>` check it was written for never executed at
    // all. Corpus entry counts have exactly one authoritative home now —
    // `nudox-store/tests/corpus_contract.rs::corpus_entry_counts_match_the_recorded_baseline`
    // and `nix/entry-baseline.toml`. Do not re-add a count check here.

    let view = IrView::with_package(descriptor.lineage.clone(), table);
    let package = Arc::new(PackageView::build(view, Provenance::TrustedLocal));
    let v = package.view();

    // `memchr` may appear more than once post-seal if the collision bug in
    // the module doc merges siblings; take every match and report all of
    // them so a failure is informative either way.
    let matches: Vec<(String, String)> = v
        .entries()
        .filter(|(_, e)| e.sym().name == "memchr")
        .filter_map(|(id, e)| match e.kind().as_owned_kind() {
            Some(Kind::Function(_)) => {
                let toks = signature::tokens(e, &package);
                Some((id.to_hex()[..12].to_owned(), render_text(&toks)))
            }
            _ => None,
        })
        .collect();

    assert!(
        !matches.is_empty(),
        "no Function entry named `memchr` found in the lowered IR"
    );

    // Print the rendered signatures on success as well as on failure. A green
    // run that shows nothing is indistinguishable from a green run against a
    // stub, and this test's whole subject is *what* got rendered.
    for (id, text) in &matches {
        eprintln!("memchr Function {id}… => {text}");
    }

    let has_option = matches.iter().any(|(_, text)| text.contains("Option"));

    assert!(
        has_option,
        "none of the {} `memchr` Function entrie(s) render with `Option` in \
         their signature — the return type's generic wrapper was dropped:\n{}\n\
         real declaration (result/memchr-2.8.3/src/memchr.rs:27):\n\
         \x20\x20pub fn memchr(needle: u8, haystack: &[u8]) -> Option<usize>",
        matches.len(),
        matches
            .iter()
            .map(|(id, text)| format!("  {id}… => {text}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}
