//! Per-module entry census for a real crate checkout.
//!
//! # Why this exists
//!
//! A total entry count is a single number, and a single number cannot say
//! *where* entries came from — which is why `memchr`'s corpus row read 11 329
//! for three consecutive generations without anyone noticing that ~10 000 of
//! those entries were `core::arch::aarch64` intrinsics swept in through
//! `memchr/src/vector.rs:294`'s private `use core::arch::aarch64::*;` glob, and
//! why 2.7.6 collapsing to 902 looked like a mystery rather than a shadowed
//! `core`. Attributing every entry to its enclosing module turns both of those
//! from "a number changed" into "these paths appeared/disappeared".
//!
//! # Running
//!
//! Every input is an *override* with a default, so the bare form runs a real
//! census over [`DEFAULT_PACKAGE`] and there is no configuration under which
//! this test does nothing:
//!
//! ```text
//! cargo test -p nudox-languages --test rust_module_census -- --ignored --nocapture
//!
//! NUDOX_PKG_ROOT=result/memchr-2.8.0 \
//! NUDOX_PKG_NAME=memchr NUDOX_PKG_VERSION=2.8.0 \
//! NUDOX_CENSUS_OUT=/tmp/memchr-2.8.0.census \
//!   cargo test -p nudox-languages --test rust_module_census -- --ignored --nocapture
//! ```
//!
//! Writes `<kind>\t<canonical path>` per entry to `NUDOX_CENSUS_OUT`, and
//! `<count>\t<owning path>` to `NUDOX_CENSUS_OUT.mods`. Two runs are compared
//! with `comm`/`diff` on the sorted entry file.
//!
//! # Why there is no skip path
//!
//! There used to be one: `NUDOX_PKG_ROOT` unset printed `SKIP:` and `return`ed,
//! while `NUDOX_PKG_NAME` and `NUDOX_CENSUS_OUT` `.expect()`ed and panicked.
//! Nothing in `.config/scripts` sets any of the three, so under
//! `--run-ignored all` this test reported PASS in 0.014 s having executed zero
//! assertions — the precise failure docs/AGENTS-DOCTRINE.md §4 condemns, and worse
//! for being asymmetric: one missing input was fatal and another was fine.
//!
//! Panicking on all three would have been symmetric and still useless, because
//! the default state of the world would then be a red test nobody could act on.
//! Defaulting all four to a checkout that is in the corpus is the honest
//! version: the test always does real work, the environment variables mean
//! "census something else instead", and a missing default checkout fails while
//! naming the path and the command that materializes it.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::PathBuf;

use nudox_ir::change::{EcosystemId, PackageLineageId, PackageName};
use nudox_languages::rust::RustProducer;
use nudox_languages::{PackageSource, produce};

/// The corpus checkout censused when nothing is overridden: directory under
/// `result/`, cargo package name, version.
///
/// `memchr` because it is the crate this file's own history is about — the
/// 11 329-entry rows and the 2.7.6 collapse to 902 — so the default run is the
/// one whose numbers a reader can compare against the record above.
const DEFAULT_PACKAGE: (&str, &str, &str) = ("memchr-2.8.3", "memchr", "2.8.3");

/// Symbols `memchr` really exports, checked only when the default package is
/// the one being censused.
///
/// Doctrine §4: the census's own three assertions are integrity checks —
/// "every entry is counted exactly once" is true of an empty table and of a
/// wrong one. These are the content check that makes a green default run mean
/// the producer read memchr, rather than that the arithmetic held.
/// `src/memchr.rs:27/92/288`, `src/lib.rs:216`.
const DEFAULT_PACKAGE_API: &[(&str, &str)] = &[
    ("Function", "memchr"),
    ("Function", "memchr2"),
    ("Record", "Memchr"),
    ("Module", "arch"),
];

#[test]
#[ignore = "drives in-process rust-analyzer over a real cargo workspace"]
fn module_census() {
    // `NUDOX_PKG_ROOT` and `NUDOX_PKG_NAME` travel together: a root with no name
    // would have to guess the cargo package from the directory, and
    // `documented_package_names` matches that string exactly, so a wrong guess
    // fails with "no documented packages found" rather than with the mistake.
    let (root, name, version, censusing_default) =
        std::env::var("NUDOX_PKG_ROOT").map_or_else(
            |_| {
                let (dir, name, version) = DEFAULT_PACKAGE;
                let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../../result")
                    .join(dir);
                (root, name.to_owned(), version.to_owned(), true)
            },
            |root| {
                (
                    PathBuf::from(root),
                    std::env::var("NUDOX_PKG_NAME").expect(
                        "NUDOX_PKG_ROOT was set without NUDOX_PKG_NAME; the cargo package name cannot \
                         be inferred from the directory (`unicode-width-0.1.11` holds package \
                         `unicode-width`, `memchr-2.8.3` holds `memchr`) and is matched exactly",
                    ),
                    std::env::var("NUDOX_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into()),
                    false,
                )
            },
        );

    assert!(
        root.join("Cargo.toml").is_file(),
        "no cargo package at {} — set NUDOX_PKG_ROOT/NUDOX_PKG_NAME to a checkout, or run \
         `nix build .#checks.corpus` to materialize the default one",
        root.display()
    );

    // Defaulted rather than `.expect()`ed for the same reason as the rest: the
    // census file is an artefact this test produces, not an input it needs from
    // its caller, and demanding it was half of the asymmetry described above.
    let out_path = std::env::var("NUDOX_CENSUS_OUT").unwrap_or_else(|_| {
        PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("{name}-{version}.census"))
            .display()
            .to_string()
    });

    let src = PackageSource::new(&root, &name, &version);
    let lineage = PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new(name.clone()));

    // Doctrine §4: every integration test is also a benchmark. This one drives
    // the single most expensive operation in the workspace and emitted no
    // `cost case=` line, so `perf-report.nu` could not see it at all.
    let (produced, _cost) =
        heart::cost::measured(&format!("census/cargo/{name}-{version}"), &root, || {
            produce(
                &RustProducer { direct_repo: false },
                &src,
                &lineage,
                &nudox_ir::foreign::Unlinked,
            )
        });

    let table = produced
        .unwrap_or_else(|e| {
            let mut cur: Option<&dyn std::error::Error> = Some(&e);
            while let Some(c) = cur {
                eprintln!("census error: {c}");
                cur = c.source();
            }
            panic!("produce failed");
        })
        .table;

    let mut lines: Vec<String> = Vec::with_capacity(table.len());
    let mut per_module: BTreeMap<String, usize> = BTreeMap::new();

    for (intro, entry) in table.iter() {
        let mut parts: Vec<String> = vec![entry.sym().name.clone()];
        let mut cur = intro;
        // Bound the walk: a cycle in `parent_of` would hang the census.
        for _ in 0..64 {
            let Some(parent_id) = table.parent_of(cur) else {
                break;
            };
            let Some(parent_entry) = table.get(parent_id) else {
                break;
            };
            parts.push(parent_entry.sym().name.clone());
            cur = parent_id;
        }
        parts.reverse();
        let kind = entry
            .kind()
            .discriminant().map_or_else(|| "Reference".to_owned(), |d| format!("{d:?}"));
        let path = parts.join("::");
        // Attribute the entry to its parent path (its enclosing module/type).
        let owner = if parts.len() > 1 {
            parts[..parts.len() - 1].join("::")
        } else {
            path.clone()
        };
        *per_module.entry(owner).or_default() += 1;
        lines.push(format!("{kind}\t{path}"));
    }

    lines.sort();
    let mut f = std::fs::File::create(&out_path).expect("create census out");
    for l in &lines {
        writeln!(f, "{l}").unwrap();
    }
    let mut mf = std::fs::File::create(format!("{out_path}.mods")).expect("create mods out");
    let mut mods: Vec<(&String, &usize)> = per_module.iter().collect();
    mods.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    for (m, c) in mods {
        writeln!(mf, "{c}\t{m}").unwrap();
    }

    // The census is only trustworthy if it accounts for the whole table — a
    // dropped entry would show up in a diff as a phantom regression in whatever
    // module happened to own it.
    assert_eq!(
        lines.len(),
        table.len(),
        "every entry in the sealed table must appear exactly once in the census"
    );
    assert!(
        per_module.values().sum::<usize>() == table.len(),
        "every entry must be attributed to exactly one owning path"
    );
    assert!(
        per_module.keys().any(|m| m == &name),
        "the package's own root must own at least one entry; a census whose paths do not \
         start at `{name}` is measuring a different crate"
    );

    // Content, not arithmetic: the three assertions above all hold for a census
    // of an empty table. These do not.
    if censusing_default {
        let missing: Vec<&(&str, &str)> = DEFAULT_PACKAGE_API
            .iter()
            .filter(|(kind, symbol)| {
                let suffix = format!("::{symbol}");
                !lines
                    .iter()
                    .any(|l| l.starts_with(&format!("{kind}\t")) && l.ends_with(&suffix))
            })
            .collect();
        assert!(
            missing.is_empty(),
            "the default census is of `{}` {}, whose own source declares these symbols; the \
             census does not contain them: {missing:?}",
            DEFAULT_PACKAGE.1,
            DEFAULT_PACKAGE.2
        );
    }

    eprintln!("census total={} out={}", table.len(), out_path);
}
