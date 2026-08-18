//! `Symbol::cfg` reaches `SymbolHead.cfg` — synthetic and real-crate coverage.
//!
//! # Why this test exists
//!
//! `nudox_ir::entry::Symbol` has always declared a `cfg` field, but until now
//! nothing between the producer and the GUI carried it: `chunk::head::head`
//! never read `entry.sym().cfg`, and `wire::SymbolHead` had no field to put it
//! in. A cfg-gated item (feature-gated, target-gated, …) rendered identically
//! to an unconditional one — the reader had no way to tell a core API from
//! one that might not exist in their build. docs.rs surfaces exactly this
//! fact as a prominent "Available on crate feature `x` only" badge; silently
//! dropping it is a real regression against the tool we claim to beat.
//!
//! This file closes that gap at the engine boundary (the wire type and the
//! GUI projection are exercised separately, in `wire::mod` doctests and
//! `symbol_page::header` unit tests respectively):
//!
//! 1. A synthetic, hand-built package proves the *plumbing*: a `Symbol` that
//!    carries a `CfgExpr` reaches `SymbolHead.cfg` rendered as Rust
//!    `#[cfg(...)]` surface syntax, and a `Symbol` with no `cfg` reaches
//!    `SymbolHead.cfg == None` (no fabricated badge).
//! 2. A real memchr lowering proves the *producer* side actually supplies
//!    `Symbol::cfg` for a genuinely target-gated public item
//!    (`memchr::arch::aarch64` / `memchr::arch::x86_64`, each declared
//!    `#[cfg(target_arch = "...")]` in `src/arch/mod.rs`), and that the whole
//!    pipeline — rust-analyzer → `nudox-languages::rust` → engine chunker —
//!    carries it through end to end.
//!
//! # A known, deliberate gap this test does NOT cover
//!
//! `memchr`'s `feature = "alloc"`-gated items (e.g.
//! `memmem::FindIter::into_owned`) were the original target for the
//! real-crate assertion below, but they never even reach the lowered IR: the
//! Rust producer's rust-analyzer workspace is loaded with **no cargo
//! features active at all** (verified by printing `Crate::cfg` from inside
//! `nudox-languages::rust::ra::item::lower` against this exact checkout — the
//! resulting `CfgOptions` contains `target_*`/`unix`/`panic=unwind`/etc. but
//! not one `feature=...` atom, even though `memchr`'s `default = ["std"]`
//! should make `std` and `alloc` active). Every `#[cfg(feature = "...")]`
//! branch is therefore dead code from rust-analyzer's point of view and its
//! items are absent from the IR entirely — not merely missing a cfg badge.
//! That is a producer workspace-loading defect in
//! `nudox-languages::rust::ra::loaded::build_cargo_config` /
//! `LoadCargoConfig`, well outside this task's scope (`cfg` plumbing through
//! the engine/wire/GUI) and outside the four files this change is allowed to
//! touch. `target_arch`-gated items are unaffected because `CfgOptions`
//! already carries the host's real target facts, which is why the assertion
//! below uses `memchr::arch::<host-arch>` instead.

use std::path::PathBuf;

use nudox_engine::store::package::{PackageView, Provenance};
use nudox_engine::store::source::producer::PackageDescriptor;
use nudox_ir::{
    apply::PristineIntroTable,
    change::{EcosystemId, IntroId, PackageLineageId, PackageName},
    entry::{CfgExpr, Entry, Node, Symbol, Visibility},
    index::RawRef,
    kind::Kind,
    kinds::Module,
    view::IrView,
};
use nudox_languages::produce;
use nudox_languages::rust::RustProducer;

use nudox_engine::chunk;

// ---------------------------------------------------------------------------
// Synthetic coverage — proves the plumbing, independent of any producer.
// ---------------------------------------------------------------------------

fn lineage(name: &str) -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("test"), PackageName::new(name))
}

fn module_sym(name: &str, cfg: Option<CfgExpr>) -> Symbol {
    Symbol {
        name: name.to_owned(),
        visibility: Visibility::Public,
        documentation: String::new(),
        source: PathBuf::new(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg,
    }
}

/// A one-entry package whose root symbol carries `cfg`.
fn single_entry_pkg(cfg: Option<CfgExpr>) -> (PackageView, IntroId) {
    let root_id = IntroId::from_raw([0x42; 32]);
    let entry = Entry::new(
        module_sym("gated_item", cfg),
        Node::build(None::<RawRef>, []),
        Kind::Module(Module),
    );

    let mut table = PristineIntroTable::new();
    table.insert_live(root_id, entry, None);

    let view = IrView::with_package(lineage("cfg-plumbing-test"), table);
    (PackageView::build(view, Provenance::TrustedLocal), root_id)
}

/// A `Symbol` carrying a `cfg` must reach `SymbolHead.cfg` populated, and
/// rendered as the full predicate (not just a "some cfg exists" marker) —
/// this is the invariant the whole task exists to establish.
///
/// Wrapped in `measured` per the house rule that every integration test also
/// doubles as a benchmark (a temp dir stands in for a package root since this
/// test never touches disk).
#[test]
fn cfg_on_symbol_reaches_symbol_head_populated() {
    let directory = std::env::temp_dir().join(format!(
        "nudox-engine-symbol-head-cfg-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&directory).expect("create measurement directory");

    let (head, cost) =
        heart::cost::measured("engine.symbol_head.cfg_populated", &directory, || {
            let (pkg, root_id) = single_entry_pkg(Some(CfgExpr::Any(Box::new([
                CfgExpr::Feature("std".to_owned()),
                CfgExpr::TargetArch("wasm32".to_owned()),
            ]))));
            let (head, _sections) =
                chunk::chunk(root_id, pkg.view(), &pkg).expect("chunk must succeed");
            head
        });

    assert_eq!(
        head.cfg.as_deref(),
        Some("cfg(any(feature = \"std\", target_arch = \"wasm32\"))"),
        "SymbolHead.cfg must carry the full rendered predicate reachable from \
         the producer's Symbol::cfg, not a stub"
    );
    assert!(cost.wall > std::time::Duration::ZERO);
    let _ = std::fs::remove_dir_all(directory);
}

/// The round-trip counterpart: a `Symbol` with no `cfg` must reach
/// `SymbolHead.cfg == None`. Without this, a bug that always fabricated a
/// badge (or always defaulted to `Some("cfg()")`) would slip through the test
/// above unnoticed.
#[test]
fn cfg_absent_on_symbol_stays_none_on_symbol_head() {
    let (pkg, root_id) = single_entry_pkg(None);
    let (head, _sections) = chunk::chunk(root_id, pkg.view(), &pkg).expect("chunk must succeed");

    assert!(
        head.cfg.is_none(),
        "an unconditional symbol must not be given a fabricated cfg badge"
    );
}

// ---------------------------------------------------------------------------
// Real-crate coverage — proves the producer side, not just the plumbing.
// ---------------------------------------------------------------------------

/// Path to the memchr checkout, mirroring `axum_root()` in
/// `real_producer_links.rs`: same env override, same default location.
fn memchr_root() -> PathBuf {
    std::env::var("NUDOX_PKG_ROOT").map_or_else(
        |_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../result/memchr-2.8.3")
                .canonicalize()
                .unwrap_or_else(|_| PathBuf::from("/nonexistent"))
        },
        PathBuf::from,
    )
}

/// A genuinely `target_arch`-gated public item from real memchr
/// (`src/arch/mod.rs`: `#[cfg(target_arch = "aarch64")] pub mod aarch64;` and
/// the symmetric `x86_64` arm) must come out of the full pipeline —
/// rust-analyzer, `nudox-languages::rust`, and `chunk::chunk` — with
/// `SymbolHead.cfg` populated with that exact predicate.
///
/// `target_arch` (unlike `feature = "..."`) is a fact rust-analyzer always
/// knows about the host it is running on, so this item reliably reaches the
/// IR regardless of the workspace-loading gap documented at the top of this
/// file. The module to look for is chosen from `std::env::consts::ARCH` so
/// the test asserts on real content on whichever architecture runs it,
/// rather than hard-coding one platform.
#[test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
fn real_memchr_arch_module_cfg_reaches_symbol_head() {
    let (mod_name, expected_cfg) = match std::env::consts::ARCH {
        "aarch64" => ("aarch64", "cfg(target_arch = \"aarch64\")"),
        "x86_64" => ("x86_64", "cfg(target_arch = \"x86_64\")"),
        other => {
            eprintln!(
                "SKIP: host arch {other:?} has no memchr::arch::<arch> submodule \
                 to assert cfg on (memchr only declares aarch64/wasm32/x86_64 arms)."
            );
            return;
        }
    };

    let root = memchr_root();
    if !root.join("Cargo.toml").is_file() {
        eprintln!(
            "SKIP: no memchr checkout at {}.\n\
             Run: scripts/fetch-real-crate.sh memchr 2.8.3",
            root.display()
        );
        return;
    }

    let descriptor = PackageDescriptor::cargo(&root, "memchr", "2.8.3");
    let case = "engine.symbol_head.real_memchr_arch_cfg";

    let (pkg, cost) = heart::cost::measured(case, &root, || {
        let table = produce(
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
            panic!("memchr must lower without error:\n{chain}");
        })
        .table;
        let view = IrView::with_package(descriptor.lineage.clone(), table);
        PackageView::build(view, Provenance::TrustedLocal)
    });

    eprintln!(
        "lowered {} entries from memchr-2.8.3 in {:.1}s",
        pkg.view().entries().count(),
        cost.wall.as_secs_f64()
    );

    let view = pkg.view();

    // Find the arch submodule by name *and* by cfg presence, so a coincidental
    // same-named entry elsewhere in the crate cannot masquerade as the real
    // target. `memchr::arch::aarch64` (or `::x86_64`) is a `Module` entry
    // directly under `memchr::arch`.
    let candidate = view
        .entries()
        .find(|(_, e)| e.sym().name == mod_name && e.sym().cfg.is_some());

    let Some((intro, entry)) = candidate else {
        panic!(
            "expected memchr to lower a `{mod_name}` module entry carrying a cfg \
             (src/arch/mod.rs declares `#[cfg(target_arch = \"{mod_name}\")] pub mod {mod_name};`), \
             but no such entry with a populated Symbol::cfg was found. \
             This is either a lowering regression or this host's rust-analyzer \
             CfgOptions no longer report target_arch=\"{mod_name}\"."
        );
    };

    assert!(
        matches!(
            entry.kind(),
            nudox_ir::entry::EntryInner::Owned(nudox_ir::kind::Kind::Module(_))
        ),
        "the `{mod_name}` entry carrying this cfg must be the arch submodule (a \
         Module), not some other same-named item"
    );

    let (head, _sections) = chunk::chunk(intro, view, &pkg).expect("chunk must succeed");

    // The breadcrumb's last two ancestors must be `memchr`, `arch` — that is
    // what pins this down as the real `memchr::arch::{aarch64,x86_64}`
    // submodule rather than a same-named decoy elsewhere in the crate. (The
    // crate-root ancestor itself is duplicated in this producer's breadcrumb
    // output — a pre-existing characteristic of how the root module's name
    // doubles with the package name — which is unrelated to cfg and out of
    // scope here; we only pin the tail of the chain.)
    let breadcrumb_labels: Vec<String> = head
        .breadcrumb
        .iter()
        .map(|c| c.label.to_string())
        .collect();
    assert!(
        breadcrumb_labels.ends_with(&["memchr".to_owned(), "arch".to_owned()]),
        "the {mod_name} module's ancestor chain must end in memchr::arch, \
         confirming this is the real arch submodule and not a same-named \
         decoy; got {breadcrumb_labels:?}"
    );

    assert_eq!(
        head.cfg.as_deref(),
        Some(expected_cfg),
        "SymbolHead.cfg for memchr::arch::{mod_name} must be the rendered \
         target_arch predicate exactly as written in src/arch/mod.rs, not a \
         placeholder or a different predicate. Symbol::cfg was {:?}",
        entry.sym().cfg,
    );
}
