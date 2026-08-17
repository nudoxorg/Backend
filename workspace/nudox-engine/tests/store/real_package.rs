//! Lower an *arbitrary* real crate, named correctly, and measure the cost.
//!
//! # Why this exists alongside `real_crate.rs`
//!
//! `real_crate.rs` is the axum-specific suite: it asserts on `Router`, on axum's
//! doc links, on axum's generics. Those assertions are valuable precisely
//! because they are specific, but they also mean the file cannot be pointed at
//! anything else — its helpers pass the literal string `"axum"` as the package
//! name regardless of what `NUDOX_REAL_CRATE_ROOT` points at.
//!
//! That is not a cosmetic problem. The package name is not a label: it is the
//! key `documented_package_names` matches against the workspace's cargo
//! metadata. Naming a checkout of `log` "axum" yields *zero* documented
//! packages, which surfaces as `RustProducerError::Load` — a failure that reads
//! like a broken producer but is really a broken invocation.
//!
//! So this file is the generic counterpart: root, name, and version are all
//! supplied together, and the assertions are the ones that hold for *any*
//! well-formed Rust crate. It is what the multi-package corpus runs.
//!
//! # Running
//!
//! ```text
//! NUDOX_PKG_ROOT=result/log-0.4.33 \
//! NUDOX_PKG_NAME=log NUDOX_PKG_VERSION=0.4.33 \
//!   cargo test -p nudox-store --test real_package -- --ignored --nocapture
//! ```
//!
//! `#[ignore]`d for the same reason as `real_crate.rs`: it drives in-process
//! rust-analyzer over a real cargo workspace, which takes tens of seconds.

use std::path::PathBuf;

use nudox_languages::produce;
use nudox_languages::rust::RustProducer;
use nudox_engine::store::package::{PackageView, Provenance};
use nudox_engine::store::source::producer::PackageDescriptor;

// ---------------------------------------------------------------------------
// Invocation
// ---------------------------------------------------------------------------

/// The three things a producer needs, kept together so they cannot drift apart.
///
/// Bundling them is the whole point of this type: the bug this file was written
/// after was a root and a name that disagreed, and a tuple of three `String`s
/// makes that just as easy to express. Here the only way to obtain one is
/// [`Target::from_env`], which reads all three or none.
#[derive(Debug, Clone)]
struct Target {
    root: PathBuf,
    name: String,
    version: String,
}

impl Target {
    /// Read the target from the environment.
    ///
    /// Returns `None` — and prints why — when the target is not configured, so
    /// an unconfigured run is a visible skip rather than a failure. A
    /// *misconfigured* run (root set, name missing) is a hard error, because
    /// that is the mistake this whole file exists to make impossible.
    fn from_env() -> Option<Self> {
        let Some(root) = var("NUDOX_PKG_ROOT") else {
            eprintln!(
                "SKIP: set NUDOX_PKG_ROOT, NUDOX_PKG_NAME and NUDOX_PKG_VERSION \
                 to point this test at a real crate checkout."
            );
            return None;
        };

        let root = PathBuf::from(root);
        assert!(
            root.join("Cargo.toml").is_file(),
            "NUDOX_PKG_ROOT={} has no Cargo.toml — a package root must be the \
             directory containing the manifest, not its parent",
            root.display(),
        );

        let name = var("NUDOX_PKG_NAME").unwrap_or_else(|| {
            panic!(
                "NUDOX_PKG_ROOT is set but NUDOX_PKG_NAME is not. The name is \
                 matched against cargo metadata to select the documented \
                 package; guessing it from the directory would reintroduce \
                 exactly the root/name mismatch this test guards against."
            )
        });

        let version = var("NUDOX_PKG_VERSION").unwrap_or_else(|| "0.0.0".to_owned());

        Some(Self {
            root,
            name,
            version,
        })
    }
}

/// Read a variable, treating exported-but-blank as unset.
fn var(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

/// A real crate lowers to a populated, well-formed IR table.
///
/// The assertions are the ones that must hold for *every* crate, so this test
/// can front the whole corpus:
///
/// * the table is non-empty — a producer that "succeeds" with zero entries is
///   indistinguishable downstream from a package with no public API;
/// * every entry has a non-empty name — a nameless entry is unsearchable and
///   renders as a blank row;
/// * no name contains a path separator — names are identifiers, not paths;
///   `PackageView`'s name index assumes this, and a `::` leaking in is how
///   search silently stops matching.
#[test]
#[ignore = "drives in-process rust-analyzer over a real cargo workspace"]
fn a_real_crate_lowers_to_a_populated_table() {
    let Some(target) = Target::from_env() else {
        return;
    };

    let descriptor = PackageDescriptor::cargo(&target.root, &target.name, &target.version);
    let case = format!("lower/{}-{}", target.name, target.version);

    let (view, cost) = heart::cost::measured(&case, &target.root, || {
        let table = produce(
            &RustProducer { direct_repo: false },
            &descriptor.source,
            &descriptor.lineage,
            &nudox_ir::foreign::Unlinked,
        )
        .unwrap_or_else(|err| {
            // Print the whole `#[source]` chain. The top-level Display of a
            // ProducerError is deliberately terse ("workspace load failed"),
            // and reading only it is what turns a five-second diagnosis into
            // an hour of guessing.
            let mut chain = format!("{err}");
            let mut cursor: &dyn std::error::Error = &err;
            while let Some(source) = std::error::Error::source(cursor) {
                chain.push_str(&format!("\n  caused by: {source}"));
                cursor = source;
            }
            panic!("{} must lower without error:\n{chain}", target.name);
        }).table;

        let ir = nudox_ir::view::IrView::with_package(descriptor.lineage.clone(), table);
        PackageView::build(ir, Provenance::TrustedLocal)
    });

    let entries: Vec<_> = view.view().entries().collect();

    assert!(
        !entries.is_empty(),
        "{} lowered to zero entries — the producer reported success but \
         produced nothing, which downstream is indistinguishable from a \
         package with no public API",
        target.name,
    );

    for (intro, entry) in &entries {
        let name = &entry.sym().name;
        assert!(
            !name.trim().is_empty(),
            "entry {intro:?} has an empty name — it would be unsearchable and \
             render as a blank row",
        );
        assert!(
            !name.contains("::") && !name.contains('/'),
            "entry name {name:?} contains a path separator; names are \
             identifiers, and the name index assumes that",
        );
    }

    eprintln!(
        "lowered {} entries from {}-{} in {:.1}s",
        entries.len(),
        target.name,
        target.version,
        cost.wall.as_secs_f64(),
    );
}
