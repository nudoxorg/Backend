//! The Implementations list for `memchr::Memchr` must name its traits.
//!
//! # The defect this pins (docs/LIMITATIONS.md L39)
//!
//! `tests/shots/memchr/08-symbol-opened.png` shipped six near-identical rows:
//!
//! ```text
//! impl ? for memchr.memchr.memchr.Memchr
//! impl ? for memchr.memchr.memchr.Memchr
//! impl<'h> ? for memchr.memchr.memchr.Memchr
//! ```
//!
//! docs.rs shows `impl Clone for Memchr`, `impl Debug for Memchr`,
//! `impl Iterator for Memchr`.
//!
//! The `?` came from `chunk::signature::resolve_nominal`'s `Ref::Local` arm —
//! three lines below a comment reading "Should not appear in a sealed table".
//! It appeared on every real package: `Lowering::refer_import` returned
//! `Ref::Local(import_index)` into an arena `IrPackage::seal` discards, and
//! nothing ever resolved it. Five of `Memchr`'s six impls name a trait in
//! `core`, so five of six rows rendered `?`.
//!
//! # The self-type half of L39
//!
//! With the trait names fixed, all six rows instead read
//! `impl Clone for memchr.memchr.memchr.Memchr` — the self type is the
//! *physical* ancestor chain (package, crate-root module, and
//! `src/memchr.rs`'s `mod memchr` are all named `memchr`) rendered verbatim.
//! `memchr_impl_labels_name_their_real_traits` below also asserts every
//! label's self type collapses to the bare `Memchr`, matching docs.rs.
//!
//! # Ground truth
//!
//! `result/memchr-2.8.3/src/memchr.rs` lines 287-351:
//!
//! * `#[derive(Clone, Debug)]` on `pub struct Memchr<'h>`  → `Clone`, `Debug`
//! * `impl<'h> Memchr<'h>`                                 → inherent, no trait
//! * `impl<'h> Iterator for Memchr<'h>`                    → `Iterator`
//! * `impl<'h> DoubleEndedIterator for Memchr<'h>`         → `DoubleEndedIterator`
//! * `impl<'h> core::iter::FusedIterator for Memchr<'h>`   → `FusedIterator`
//!
//! # Running
//!
//! ```text
//! cargo test -p nudox-engine --test real_memchr_impl_labels -- --ignored --nocapture
//! ```

use std::{collections::BTreeSet, path::PathBuf, sync::Arc};

use nudox_ir::{kind::Kind, view::IrView};
use nudox_languages::produce;
use nudox_languages::rust::RustProducer;
use nudox_engine::store::{
    package::{PackageView, Provenance},
    source::producer::PackageDescriptor,
};

use nudox_engine::{chunk::signature, wire::SigToken};

fn var(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

fn root() -> PathBuf {
    var("NUDOX_PKG_ROOT").map_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../result/memchr-2.8.3")
    }, PathBuf::from)
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

/// The six impls on `Memchr` must render six *distinct* trait names matching
/// the source, and none may render `?`.
///
/// Asserts on the set of trait names, not on a count: a count of six is
/// satisfied by six rows that all say `?`, which is exactly what shipped.
#[test]
#[ignore = "drives in-process rust-analyzer over a real cargo workspace (~20-45 s); \
            run explicitly with --ignored"]
fn memchr_impl_labels_name_their_real_traits() {
    let root = root();
    assert!(
        root.join("Cargo.toml").is_file(),
        "no memchr checkout at {}; set NUDOX_PKG_ROOT or fetch the fixture",
        root.display()
    );

    let descriptor = PackageDescriptor::cargo(&root, "memchr", "2.8.3");
    let (produced, cost) =
        heart::cost::measured("l39/memchr-2.8.3/impl-labels", &root, || {
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
                panic!("memchr must lower without error:\n{chain}");
            })
        });

    let report = &produced.report;
    // Every colliding group of size N used to collapse to one entry, so
    // `sum(group) - group_count` is exactly the number of declarations that
    // used to be dropped on the floor by `HashMap::insert`.
    let recovered: usize = report
        .forced
        .iter()
        .map(|f| f.group.saturating_sub(1))
        .sum();
    eprintln!(
        "sealed {} entries in {:.1}s — {} foreign keys linked, {} unlinked, \
         {} forced groups recovering {} declarations that previously vanished, \
         {} unmapped locals, {} lost collisions",
        produced.table.len(),
        cost.wall.as_secs_f64(),
        report.linked.len(),
        report.unlinked.len(),
        report.forced.len(),
        recovered,
        report.unmapped_local.len(),
        report.collisions.len(),
    );
    // Naming a foreign type is what makes the reference renderable at all; if
    // memchr stopped producing any, the `?` would be back.
    assert!(
        !report.unlinked.is_empty(),
        "memchr names `core` traits, so seal must observe cross-package keys"
    );

    // An arena-local ref surviving seal is the original defect. It is now
    // reportable rather than silent, so assert it is empty.
    assert!(
        report.unmapped_local.is_empty(),
        "arena-local refs survived seal: {:?}",
        report.unmapped_local
    );
    assert!(
        report.collisions.is_empty(),
        "declarations were lost to identity collisions after escalation: {:?}",
        report
            .collisions
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
    );

    let view = IrView::with_package(descriptor.lineage, produced.table);
    let package = Arc::new(PackageView::build(view, Provenance::TrustedLocal));
    let v = package.view();

    // Locate the `Memchr` struct declared in `src/memchr.rs`. There are several
    // `Memchr*` types in the crate; match the exact name.
    let memchr_ty = v
        .entries()
        .filter(|(_, e)| e.sym().name == "Memchr")
        .find(|(_, e)| matches!(e.kind().as_owned_kind(), Some(Kind::Record(_))))
        .map(|(id, _)| id)
        .expect("memchr declares `pub struct Memchr<'h>`");

    // Every impl whose self type is that struct.
    let mut labels: Vec<String> = Vec::new();
    let mut trait_names: BTreeSet<String> = BTreeSet::new();
    let mut inherent = 0usize;

    for (id, entry) in v.entries() {
        let Some(Kind::Impl(i)) = entry.kind().as_owned_kind() else {
            continue;
        };
        let self_is_memchr = match &i.self_ty {
            nudox_ir::kinds::ty::Type::Nominal(nudox_ir::index::Ref::Intro(x)) => *x == memchr_ty,
            nudox_ir::kinds::ty::Type::Apply { base, .. } => matches!(
                base.as_ref(),
                nudox_ir::kinds::ty::Type::Nominal(nudox_ir::index::Ref::Intro(x)) if *x == memchr_ty
            ),
            _ => false,
        };
        if !self_is_memchr {
            continue;
        }
        let _ = id;

        let label = render_text(&signature::tokens(entry, &package));
        labels.push(label.clone());

        match &i.of {
            None => inherent += 1,
            Some(of) => {
                // Assert on the IR, not only on the rendered string: the trait
                // must be a `Ref::Foreign` that *names* `core`, which is the
                // thing that was structurally impossible before.
                let base = match of {
                    nudox_ir::kinds::ty::Type::Apply { base, .. } => base.as_ref(),
                    other => other,
                };
                let nudox_ir::kinds::ty::Type::Nominal(raw) = base else {
                    panic!("an impl's trait must be a nominal type; got {base:?}");
                };
                match raw {
                    nudox_ir::index::Ref::Foreign { key, .. } => {
                        trait_names.insert(key.display.to_string());
                    }
                    nudox_ir::index::Ref::Intro(_) => {
                        // A same-package trait: `Memchr` implements none, so
                        // this would mean the self-type filter is wrong.
                        panic!("unexpected same-package trait on Memchr: {label}");
                    }
                    nudox_ir::index::Ref::Local(_) => panic!(
                        "a dangling arena index survived seal — this is the \
                         defect itself, and it renders as `?`. Label: {label}"
                    ),
                }
            }
        }
    }

    labels.sort();
    eprintln!("--- Implementations for memchr::Memchr ---");
    for l in &labels {
        eprintln!("{l}");
    }
    eprintln!("------------------------------------------");

    assert_eq!(
        inherent, 1,
        "exactly one inherent impl (`impl<'h> Memchr<'h>`, src/memchr.rs:293)"
    );

    let expected: BTreeSet<String> = [
        "Clone",
        "Debug",
        "DoubleEndedIterator",
        "FusedIterator",
        "Iterator",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();

    assert_eq!(
        trait_names, expected,
        "the five trait impls on `Memchr` must name their real traits, verified \
         against result/memchr-2.8.3/src/memchr.rs:287-351"
    );

    assert_eq!(
        labels.len(),
        6,
        "six impls total: two derives, one inherent, and three hand-written"
    );
    assert!(
        !labels.iter().any(|l| l.contains('?')),
        "no impl label may contain `?`; got {labels:?}"
    );

    // L18/L39, self-type half: `memchr`'s package, crate-root module, and
    // `mod memchr;` (src/memchr.rs) are all named `memchr`, three deep ahead
    // of the struct — `path_of` really does report
    // `memchr.memchr.memchr.Memchr`. `Memchr` is the only type of that name
    // in the crate, so nothing needs to survive to disambiguate it: every
    // label's self type must collapse to the bare leaf, exactly what
    // docs.rs shows (`impl Clone for Memchr`, not
    // `impl Clone for memchr.memchr.memchr.Memchr`).
    for l in &labels {
        assert!(
            l.ends_with("Memchr"),
            "self type must be the trailing token of the impl label: {l}"
        );
        assert!(
            !l.contains("memchr."),
            "self type must render as the bare leaf `Memchr`, not a \
             package-qualified or repeated-segment ancestor path: {l}"
        );
    }
}
