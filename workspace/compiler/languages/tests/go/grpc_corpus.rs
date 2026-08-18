//! Dedicated corpus coverage for `google.golang.org/grpc` — the full
//! `nudox_languages::produce` pipeline (`invoke` -> `lower` -> `finish` ->
//! `seal`) against a real, pinned checkout under `result/`.
//!
//! # Why a dedicated file, not another `corpus_sweep.rs` entry
//!
//! `corpus_sweep.rs` sweeps 22 lighter entries (the largest, `gin`, is a
//! few dozen files) in one test function; grpc is a different order of
//! magnitude — 321 packages, ~30 external module dependencies (xds,
//! opentelemetry, spiffe, envoy's go-control-plane, …) that all need to be
//! present in `GOMODCACHE` for `go/packages.Load` to type-check the module
//! offline. Folding it into the sweep would mean every run of that test —
//! including ones only exercising the other 22, lighter entries during
//! unrelated iteration — pays grpc's full load-and-typecheck cost, and a
//! grpc-only failure would sit interleaved with (and be easy to miss
//! among) 22 unrelated per-entry outcomes. Keeping it separate lets it be
//! run, or skipped, independently.
//!
//! # Regression: promoted method vs. local field name collision
//!
//! This corpus entry is also a real, hand-verified regression case for a
//! defect found while adding it: `google.golang.org/grpc/xds`'s unexported
//! `serverOption` struct declares its own field named `apply` *and* embeds
//! `grpc.EmptyServerOption`, whose unexported `apply` method gets promoted.
//! Both are named `apply`, but per the Go spec's "Uniqueness of
//! identifiers" rule, two unexported identifiers from *different* packages
//! are different identifiers even when spelled the same — so this is
//! legal, unambiguous Go, and `go/types.NewMethodSet` (the oracle's
//! `promotedMethods`, oracle/go/serialize.go) correctly reports both the
//! field and the promoted method. The lowering layer's `GoId::Member` id
//! is a flat `(import_path, type_name, member_name)` string triple with no
//! notion of that per-package unexported-identifier distinction, so
//! declaring both under the same id used to panic `Lowering::finish` with
//! "declared more than once". Fixed in `src/go/lower/mod.rs`'s
//! `lower_methods`: a promoted method whose name collides with the type's
//! own field or directly-declared method is skipped (the local one always
//! wins Go's own selector resolution; the promoted one is never reachable
//! via `T.apply` from outside its defining package). This test's
//! `produce()` call is the only corpus entry that reaches that code path
//! with a real collision — losing this entry loses that regression
//! coverage.

use std::path::{Path, PathBuf};

use nudox_ir::change::{EcosystemId, PackageLineageId, PackageName};
use nudox_languages::go::producer::GoProducer;
use nudox_languages::{PackageSource, produce};

const MODULE: &str = "google.golang.org/grpc";
const VERSION: &str = "v1.83.0";
/// Mirrors `flake.nix`'s `safeDirName`: `/` and `:` both become `__`.
const DIR: &str = "google.golang.org__grpc-v1.83.0";

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../result")
        .join(DIR)
        .canonicalize()
        .expect("no result/google.golang.org__grpc-v1.83.0 checkout — see docs/CORPUS.md")
}

fn oracle_bin() -> PathBuf {
    let oracle_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("oracle/go");
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("nudox-go-oracle-grpc");
    let status = std::process::Command::new("go")
        .current_dir(&oracle_dir)
        .arg("build")
        .arg("-o")
        .arg(&out)
        .arg("./...")
        .status()
        .expect("`go` must be on PATH to build the oracle");
    assert!(status.success(), "go build of the oracle failed");
    out
}

/// Walk the full `std::error::Error` source chain (docs/AGENTS-DOCTRINE.md's
/// "hard-won facts": the top-level `Display` alone is deliberately terse).
fn chain(e: &(dyn std::error::Error + 'static)) -> String {
    let mut out = e.to_string();
    let mut cur: Option<&(dyn std::error::Error + 'static)> = e.source();
    while let Some(src) = cur {
        out.push_str(" <- ");
        out.push_str(&src.to_string());
        cur = src.source();
    }
    out
}

#[test]
fn grpc_go_lowers_through_the_real_producer() {
    let root = corpus_root();
    assert!(
        root.join("go.mod").is_file(),
        "no go.mod at {} — the grpc checkout is missing or incomplete",
        root.display()
    );

    // SAFETY: single-value env vars, read immediately by the subprocess this
    // call spawns; no other test in this binary races on them (same
    // justification as `corpus_sweep.rs`/`real_package.rs`).
    unsafe {
        std::env::set_var("NUDOX_GO_ORACLE_BIN", oracle_bin());
    }
    // Force fully-offline resolution against the pre-populated GOMODCACHE —
    // same requirement, and same provisioning mechanism, as every other Go
    // corpus entry (see `corpus_sweep.rs`). grpc's dependency graph is far
    // larger than the other 22 entries', so this is where a stale or
    // incomplete module cache would surface first.
    unsafe {
        std::env::set_var("GOPROXY", "off");
        std::env::set_var("GOSUMDB", "off");
        std::env::set_var("GOFLAGS", "-mod=mod");
    }

    // Independent oracle decl-count floor: a SEPARATE oracle invocation from
    // the one `produce()` makes internally (docs/AGENTS-DOCTRINE.md §4).
    let oracle_output = GoProducer
        .invoke_oracle(&root)
        .unwrap_or_else(|e| panic!("oracle (independent verification run) failed: {}", chain(&e)));
    assert!(
        oracle_output.errors.is_empty(),
        "oracle reported errors: {:?}",
        oracle_output.errors
    );
    assert!(
        oracle_output.packages.len() > 200,
        "expected well over 200 packages for grpc-go v1.83.0 (321 measured at pin time); got {}",
        oracle_output.packages.len()
    );
    let oracle_decls: usize = oracle_output.packages.iter().map(|p| p.decls.len()).sum();

    let src = PackageSource::new(&root, MODULE, VERSION);
    let lid = PackageLineageId::new(EcosystemId::new("go"), PackageName::new(MODULE));

    let (produced, _cost) = heart::cost::measured("go-grpc-corpus", &root, || {
        produce(&GoProducer, &src, &lid, &nudox_ir::foreign::Unlinked)
    });

    let produced = produced.unwrap_or_else(|e| panic!("produce() failed: {}", chain(&e)));

    // Real-content floor, not a bare non-zero check: see corpus_sweep.rs's
    // identical reasoning — the table also nests fields/methods/params/
    // variants under package-level decls, so it can only be >= oracle_decls.
    assert!(
        produced.table.len() >= oracle_decls,
        "table has fewer live entries ({}) than the oracle's own independently-counted \
         decls ({oracle_decls}) — lowering dropped symbols",
        produced.table.len()
    );

    eprintln!(
        "lowered {} live entries from {MODULE}@{VERSION} ({} packages, {oracle_decls} oracle \
         decls) via the real oracle; {} unlinked refs",
        produced.table.len(),
        oracle_output.packages.len(),
        produced.report.unlinked.len()
    );
}
