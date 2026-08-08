//! L23 regression guard — a parameter's identity must be *derived from its
//! owning function*, not from its position among unrelated sibling functions
//! declared in the same module.
//!
//! # The defect
//!
//! `workspace/compiler/languages/rust/src/ra/item.rs`'s `declare_params` and
//! the `output_refs` closure inside `lower_free_function_with_id` used to
//! declare every `Param` entry (inputs *and* the synthetic `return` output
//! param) under the function's *enclosing* module/impl id, not under the
//! function's own id. `workspace/ir/model/src/package/seal.rs` derives a
//! param's collision key from `(kind, ancestor-path, leaf-name)`, walking
//! declared-parent pointers — so with the module as parent, two sibling
//! functions' same-named params were indistinguishable at that key. Every
//! other `Kind` falls to `Disambiguator::Span` on collision, and every
//! producer-synthesized param carries `span: 0..0` (`item.rs::plain_sym`), so
//! that "disambiguator" was identical for every colliding param too. What
//! actually kept those collisions from silently overwriting each other was
//! `seal.rs`'s general escalation pass (`Disambiguator::Span` → `Ordinal`) —
//! a backstop that assigns *distinct* ids to colliding entries by their
//! **declaration-order position within the colliding group**.
//!
//! # Why this test, not just "do the ids differ"
//!
//! Escalation alone already makes same-module siblings' params come out
//! distinct — so a test that just checks "alpha's `needle` != beta's
//! `needle`" would pass under the broken code too, and would prove nothing.
//! The actual defect is that an ordinal is a position, not an identity: it
//! shifts if an unrelated sibling function is declared earlier in the same
//! module, even though nothing about the param being examined changed.
//!
//! So the real invariant is stability under an irrelevant edit: declaring a
//! **new**, unrelated sibling function earlier in the module must not change
//! the `IntroId` of `alpha`'s or `beta`'s params. That is exactly what
//! ordinal escalation cannot promise (it is order-dependent by
//! construction), and exactly what parenting a param on its owning function
//! does promise (the function's name is now part of the param's
//! ancestor-path, so it is unique on its own merits, independent of how many
//! siblings exist or in what order).
//!
//! # Running
//!
//! ```text
//! cargo test -p nudox-producer-rust --test param_identity
//! ```

use std::path::{Path, PathBuf};

use nudox_ir::{
    apply::PristineIntroTable,
    change::{EcosystemId, IntroId, PackageLineageId, PackageName},
    entry::{EntryInner, Symbol, Visibility},
    foreign::Unlinked,
    index::Ref,
    kind::Kind,
    lower::Lowering,
    package::PackageId,
};
use nudox_producer::{PackageSource, Producer};
use nudox_producer_rust::RustProducer;

// ── Fixtures ──────────────────────────────────────────────────────────────────

/// Two sibling free functions sharing input-param names (`needle`,
/// `haystack`) and the synthetic `return` output-param name — the exact
/// collision shape `real_memchr_generic_return.rs`'s module doc describes for
/// `memchr`/`memchr2`/… (18-way in the real crate; 2-way is the minimal
/// reproduction).
const TWO_SIBLINGS: &str = r#"
pub fn alpha(needle: u8, haystack: &[u8]) -> Option<usize> {
    let _ = (needle, haystack);
    None
}

pub fn beta(needle: u8, haystack: &[u8]) -> Option<usize> {
    let _ = (needle, haystack);
    None
}
"#;

/// The same two functions, plus a **third, unrelated** sibling (`gamma`)
/// declared *before* both of them and sharing the same param names.
///
/// This is the irrelevant edit: nothing about `alpha` or `beta` changed, but
/// under ordinal escalation a new colliding member inserted earlier in
/// declaration order shifts every later member's ordinal — and therefore its
/// `IntroId` — even though the sibling has nothing to do with either
/// function.
const THREE_SIBLINGS_NEW_ONE_FIRST: &str = r#"
pub fn gamma(needle: u8, haystack: &[u8]) -> Option<usize> {
    let _ = (needle, haystack);
    None
}

pub fn alpha(needle: u8, haystack: &[u8]) -> Option<usize> {
    let _ = (needle, haystack);
    None
}

pub fn beta(needle: u8, haystack: &[u8]) -> Option<usize> {
    let _ = (needle, haystack);
    None
}
"#;

// ── Harness ───────────────────────────────────────────────────────────────────

/// Write a standalone cargo package under `CARGO_TARGET_TMPDIR`.
///
/// The trailing empty `[workspace]` table is mandatory: the fixture lives
/// under `target/`, inside this repo's own cargo workspace, and without it
/// `cargo metadata` fails with "current package believes it's in a workspace
/// when it's not" (doctrine §8) — a *load* failure that would masquerade as
/// something else entirely.
/// Write a fixture under on-disk directory `dir`, with the *Cargo package
/// itself* named `pkg_name`.
///
/// These are deliberately independent: `dir` only needs to be unique so two
/// on-disk checkouts do not collide; `pkg_name` becomes the crate's own root
/// module name inside the lowered tree (rust-analyzer nests a
/// `ModuleDef::Module` for the crate root, one level below the caller's
/// `Lowering` wrapper root, named after the crate). That name is therefore a
/// real ancestor-path segment for *every* entry in the crate — including
/// `alpha`'s params — and feeds `IntroId`. The first version of this harness
/// derived `pkg_name` from `dir` to keep checkouts unique, which made two
/// "identical" fixtures written to different directories seal to different
/// ids for a reason that had nothing to do with param identity. Two
/// lowerings being compared for id stability must share `pkg_name`.
fn write_fixture(dir: &str, pkg_name: &str, body: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(dir);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).expect("create fixture src dir");

    std::fs::write(
        root.join("Cargo.toml"),
        format!(
            "[package]\n\
             name = \"{pkg_name}\"\n\
             version = \"0.1.0\"\n\
             edition = \"2021\"\n\
             \n\
             [lib]\n\
             path = \"src/lib.rs\"\n\
             \n\
             [workspace]\n"
        ),
    )
    .expect("write fixture manifest");
    std::fs::write(root.join("src/lib.rs"), body).expect("write fixture lib.rs");
    root
}

fn source_for(root: &Path, pkg_name: &str) -> PackageSource {
    PackageSource::new(root, pkg_name, "0.1.0")
}

/// Produce and seal `body` under a fixture written to on-disk `dir`, whose
/// Cargo package (and therefore lowered crate-root module, and `seal`
/// lineage) is named `pkg_name`.
///
/// See [`write_fixture`] for why `dir` and `pkg_name` must be independent:
/// two lowerings being compared for id stability must share `pkg_name`, or
/// every id differs trivially by crate-root name and the comparison proves
/// nothing about param identity.
fn lower(dir: &str, pkg_name: &str, body: &str) -> PristineIntroTable {
    let root = write_fixture(dir, pkg_name, body);
    let src = source_for(&root, pkg_name);
    let producer = RustProducer { direct_repo: false };

    let oracle = producer
        .invoke(&src)
        .unwrap_or_else(|e| panic!("fixture {dir} must load: {e}"));

    let root_sym = Symbol {
        name: pkg_name.to_owned(),
        visibility: Visibility::Public,
        documentation: String::new(),
        source: src.root.clone(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    };
    let mut sink: Lowering<_> = Lowering::new(PackageId::path(src.root()), root_sym);
    producer
        .lower(&oracle, &mut sink)
        .unwrap_or_else(|e| panic!("fixture {dir} must lower: {e}"));
    let package = sink
        .finish()
        .unwrap_or_else(|e| panic!("fixture {dir} lowering must be structurally sound: {e}"));

    let lineage =
        PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new(pkg_name.to_owned()));
    package.seal(&lineage, &Unlinked).table
}

/// Find the `IntroId` of the param named `param_name` (input or output) that
/// belongs to the `Function` entry named `fn_name`.
///
/// Panics with a descriptive message if either is missing — a missing
/// function or param is itself informative (doctrine §4: assert on real
/// content, not on a non-zero count).
fn param_intro(table: &PristineIntroTable, fn_name: &str, param_name: &str) -> IntroId {
    let (_, func_entry) = table
        .iter()
        .find(|(_, e)| {
            e.sym().name == fn_name && matches!(e.kind(), EntryInner::Owned(Kind::Function(_)))
        })
        .unwrap_or_else(|| panic!("no Function entry named `{fn_name}` in the sealed table"));

    let EntryInner::Owned(Kind::Function(f)) = func_entry.kind() else {
        unreachable!("filtered above")
    };

    f.input_params
        .iter()
        .chain(f.output_params.iter())
        .find_map(|r| match r {
            Ref::Intro(id) => {
                let entry = table.get(*id)?;
                (entry.sym().name == param_name).then_some(*id)
            }
            other => panic!(
                "`{fn_name}`'s param ref did not seal to Ref::Intro: {other:?} \
                 (SealReport::unmapped_local would explain this)"
            ),
        })
        .unwrap_or_else(|| {
            panic!("`{fn_name}` has no param named `{param_name}` in the sealed table")
        })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Sibling functions' same-named params must seal to distinct `IntroId`s.
///
/// This alone does not distinguish a real fix from the escalation-pass mask
/// (see module doc) — it is here as a basic sanity check before the stronger
/// assertion below.
#[test]
fn sibling_functions_same_named_params_get_distinct_intro_ids() {
    let table = lower(
        "nudox_fixture_param_identity_basic",
        "nudox_fixture_param_identity_basic",
        TWO_SIBLINGS,
    );

    let alpha_needle = param_intro(&table, "alpha", "needle");
    let beta_needle = param_intro(&table, "beta", "needle");
    assert_ne!(
        alpha_needle, beta_needle,
        "alpha's and beta's `needle` params collided onto one IntroId"
    );

    let alpha_return = param_intro(&table, "alpha", "return");
    let beta_return = param_intro(&table, "beta", "return");
    assert_ne!(
        alpha_return, beta_return,
        "alpha's and beta's `return` params collided onto one IntroId"
    );
}

/// The invariant that actually distinguishes a real fix from the
/// escalation-pass mask: declaring a new, unrelated sibling function earlier
/// in the same module must not change `alpha`'s or `beta`'s param
/// `IntroId`s.
///
/// Under the pre-fix code, `alpha`'s and `beta`'s params are parented on the
/// enclosing module, so their collision-group membership (and thus the
/// `Disambiguator::Ordinal` index minted for them) depends on how many
/// same-named sibling params exist and in what declaration order — inserting
/// `gamma` earlier shifts that index for every later member. Under the fix,
/// each param's ancestor-path includes its owning function's name, so its
/// identity does not depend on `gamma` existing at all.
#[test]
fn a_new_unrelated_sibling_does_not_change_existing_functions_param_ids() {
    let without_gamma = lower(
        "nudox_fixture_param_identity_two",
        "nudox_fixture_param_identity_shared",
        TWO_SIBLINGS,
    );
    let with_gamma = lower(
        "nudox_fixture_param_identity_three",
        "nudox_fixture_param_identity_shared",
        THREE_SIBLINGS_NEW_ONE_FIRST,
    );

    for (fn_name, param_name) in [
        ("alpha", "needle"),
        ("alpha", "haystack"),
        ("alpha", "return"),
        ("beta", "needle"),
        ("beta", "haystack"),
        ("beta", "return"),
    ] {
        let before = param_intro(&without_gamma, fn_name, param_name);
        let after = param_intro(&with_gamma, fn_name, param_name);
        assert_eq!(
            before, after,
            "`{fn_name}`'s `{param_name}` param changed IntroId merely because \
             an unrelated sibling function (`gamma`) was declared earlier in \
             the same module — its identity is positional, not derived from \
             its owning function"
        );
    }
}

/// Determinism: lowering and sealing the exact same source twice must yield
/// the exact same `IntroId` for a given param. A real fix and the escalation
/// mask both satisfy this in isolation (escalation is deterministic given a
/// fixed declaration order) — it is included because doctrine requires
/// determinism be checked explicitly, not assumed, and because it is the
/// weaker half of the pair the task record asks for; the stronger half is
/// `a_new_unrelated_sibling_does_not_change_existing_functions_param_ids`
/// above.
#[test]
fn the_same_param_gets_the_same_id_across_two_independent_lowerings() {
    let first = lower(
        "nudox_fixture_param_identity_det_a",
        "nudox_fixture_param_identity_det",
        TWO_SIBLINGS,
    );
    let second = lower(
        "nudox_fixture_param_identity_det_b",
        "nudox_fixture_param_identity_det",
        TWO_SIBLINGS,
    );

    for (fn_name, param_name) in [
        ("alpha", "needle"),
        ("alpha", "haystack"),
        ("alpha", "return"),
        ("beta", "needle"),
        ("beta", "haystack"),
        ("beta", "return"),
    ] {
        let a = param_intro(&first, fn_name, param_name);
        let b = param_intro(&second, fn_name, param_name);
        assert_eq!(
            a, b,
            "`{fn_name}`'s `{param_name}` param got a different IntroId across \
             two independent lowerings of identical source"
        );
    }
}
