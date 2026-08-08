//! Sweeps every provisioned `go` corpus package through the full
//! `nudox_producer::produce` pipeline (`invoke` -> `lower` -> `finish` ->
//! `seal`) against real, third-party module checkouts under `.real-crates/`.
//!
//! `real_package.rs` proves the pipeline works end to end on one package
//! (`go.uber.org/zap`) with deep content assertions. This file proves it
//! (or does not) on the other 20 corpus packages / 22 version entries, per
//! AGENTS-DOCTRINE.md §4's warning that a producer's hand-fixture test suite
//! tells you nothing about real code: the Go producer's own history includes
//! 47 green fixture tests that crashed on the first real package.
//!
//! Every entry is measured (`nudox_test_support::measured`, doctrine §4) and
//! every assertion is on real content: the resulting `PristineIntroTable`'s
//! live-entry count is checked against an independently-derived floor (the
//! oracle's own per-package `decls` count, summed, and re-derived by a
//! *second*, separate invocation of the oracle binary — not the one
//! `produce()` made internally) rather than against a bare non-zero check,
//! and the oracle's own `errors` array is asserted empty.
//!
//! This intentionally does not abort on the first failure: the whole point
//! of a first sweep is to find every defect in one pass, not stop at the
//! first one. Each entry's outcome is recorded and printed; a summary
//! assertion at the end fails the test if anything did not succeed, with
//! the full `std::error::Error` source chain for every failure (never just
//! the terse top-level `Display` — AGENTS-DOCTRINE.md's "hard-won facts").

use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

use nudox_ir::change::{EcosystemId, PackageLineageId, PackageName};
use nudox_producer::{PackageSource, produce};
use nudox_producer_go::producer::GoProducer;

/// One corpus entry: `.real-crates/<dir>`, the Go module path used both as
/// the on-disk name-selector-equivalent and the `PackageLineageId`, and the
/// ecosystem version string. Mirrors `corpus/manifest.toml`'s 20 `go`
/// `[[packages]]` blocks / 22 version entries exactly (`pkg/errors` and
/// `stretchr/testify` each carry two versions for lineage testing).
struct Entry {
    dir: &'static str,
    module: &'static str,
    version: &'static str,
}

const ENTRIES: &[Entry] = &[
    Entry { dir: "github.com__pkg__errors-v0.8.1", module: "github.com/pkg/errors", version: "v0.8.1" },
    Entry { dir: "github.com__pkg__errors-v0.9.1", module: "github.com/pkg/errors", version: "v0.9.1" },
    Entry { dir: "github.com__google__uuid-v1.6.0", module: "github.com/google/uuid", version: "v1.6.0" },
    Entry { dir: "github.com__spf13__cobra-v1.10.2", module: "github.com/spf13/cobra", version: "v1.10.2" },
    Entry { dir: "github.com__spf13__viper-v1.21.0", module: "github.com/spf13/viper", version: "v1.21.0" },
    Entry { dir: "github.com__stretchr__testify-v1.11.1", module: "github.com/stretchr/testify", version: "v1.11.1" },
    Entry { dir: "github.com__stretchr__testify-v1.9.0", module: "github.com/stretchr/testify", version: "v1.9.0" },
    Entry { dir: "gopkg.in__yaml.v3-v3.0.1", module: "gopkg.in/yaml.v3", version: "v3.0.1" },
    Entry { dir: "github.com__gin-gonic__gin-v1.12.0", module: "github.com/gin-gonic/gin", version: "v1.12.0" },
    Entry { dir: "golang.org__x__sync-v0.22.0", module: "golang.org/x/sync", version: "v0.22.0" },
    Entry { dir: "golang.org__x__text-v0.40.0", module: "golang.org/x/text", version: "v0.40.0" },
    Entry { dir: "github.com__prometheus__client_golang-v1.24.1", module: "github.com/prometheus/client_golang", version: "v1.24.1" },
    Entry { dir: "github.com__redis__go-redis__v9-v9.22.0", module: "github.com/redis/go-redis/v9", version: "v9.22.0" },
    Entry { dir: "github.com__hashicorp__go-multierror-v1.1.1", module: "github.com/hashicorp/go-multierror", version: "v1.1.1" },
    Entry { dir: "github.com__BurntSushi__toml-v1.6.0", module: "github.com/BurntSushi/toml", version: "v1.6.0" },
    Entry { dir: "github.com__mattn__go-sqlite3-v1.14.49", module: "github.com/mattn/go-sqlite3", version: "v1.14.49" },
    Entry { dir: "github.com__golang-jwt__jwt__v5-v5.3.1", module: "github.com/golang-jwt/jwt/v5", version: "v5.3.1" },
    Entry { dir: "github.com__google__go-cmp-v0.7.0", module: "github.com/google/go-cmp", version: "v0.7.0" },
    Entry { dir: "github.com__robfig__cron__v3-v3.0.1", module: "github.com/robfig/cron/v3", version: "v3.0.1" },
    Entry { dir: "github.com__fsnotify__fsnotify-v1.10.1", module: "github.com/fsnotify/fsnotify", version: "v1.10.1" },
    Entry { dir: "github.com__samber__lo-v1.53.0", module: "github.com/samber/lo", version: "v1.53.0" },
    Entry { dir: "go.uber.org__zap-v1.28.0", module: "go.uber.org/zap", version: "v1.28.0" },
];

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../.real-crates")
        .canonicalize()
        .expect("no .real-crates/ checkout — see corpus/README.md to (re)provision it")
}

/// Build the oracle binary once, exactly as `real_package.rs` does.
fn oracle_bin() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let oracle_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("oracle");
        let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("nudox-go-oracle-sweep");
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
    })
}

/// Walk the full `std::error::Error` source chain. The top-level `Display`
/// on `ProducerError`/this crate's `Error` is deliberately terse
/// (AGENTS-DOCTRINE.md's "hard-won facts": reading only it turns a
/// five-second diagnosis into an hour).
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

enum Outcome {
    Ok {
        table_len: usize,
        oracle_packages: usize,
        oracle_decls: usize,
        unlinked: usize,
    },
    Fail {
        stage: &'static str,
        chain: String,
    },
}

fn run_entry(entry: &Entry) -> Outcome {
    let root = corpus_root().join(entry.dir);
    if !root.join("go.mod").is_file() {
        return Outcome::Fail {
            stage: "preflight",
            chain: format!("no go.mod at {}", root.display()),
        };
    }

    // SAFETY: single-value env var read immediately by the subprocess this
    // call spawns; tests in this binary run single-threaded from the caller's
    // perspective because each entry sets the same value before using it (see
    // `real_package.rs` for the identical justification).
    unsafe {
        std::env::set_var("NUDOX_GO_ORACLE_BIN", oracle_bin());
    }
    // Force fully-offline resolution against the pre-populated GOMODCACHE —
    // the same sentinel provisioning's final verification pass used. A
    // silent fall-through to the network would invalidate the "this works
    // offline" claim the corpus exists to support.
    unsafe {
        std::env::set_var("GOPROXY", "off");
        std::env::set_var("GOSUMDB", "off");
        std::env::set_var("GOFLAGS", "-mod=mod");
    }

    // Independent oracle decl-count floor: a SEPARATE invocation from the one
    // `produce()` makes internally, via the inherent (non-trait) method, so
    // this number cannot be silently inflated or deflated by whatever
    // `produce()`'s own invocation happens to do.
    let oracle_output = match GoProducer.invoke_oracle(&root) {
        Ok(o) => o,
        Err(e) => {
            return Outcome::Fail {
                stage: "oracle (independent verification run)",
                chain: chain(&e),
            };
        }
    };
    if !oracle_output.errors.is_empty() {
        return Outcome::Fail {
            stage: "oracle diagnostics",
            chain: format!("oracle reported errors: {:?}", oracle_output.errors),
        };
    }
    let oracle_packages = oracle_output.packages.len();
    let oracle_decls: usize = oracle_output.packages.iter().map(|p| p.decls.len()).sum();

    let src = PackageSource::new(&root, entry.module, entry.version);
    let lid = PackageLineageId::new(EcosystemId::new("go"), PackageName::new(entry.module));

    let case = format!("go-sweep-{}", entry.dir);
    let (produced, _cost) =
        nudox_test_support::measured(&case, &root, || produce(&GoProducer, &src, &lid, &nudox_ir::foreign::Unlinked));

    match produced {
        Ok(p) => Outcome::Ok {
            table_len: p.table.len(),
            oracle_packages,
            oracle_decls,
            unlinked: p.report.unlinked.len(),
        },
        Err(e) => Outcome::Fail {
            stage: "produce (invoke/lower/finish/seal)",
            chain: chain(&e),
        },
    }
}

#[test]
fn every_provisioned_go_corpus_package_lowers_through_the_real_producer() {
    let mut failures = Vec::new();
    let mut successes = Vec::new();

    for entry in ENTRIES {
        eprintln!("=== {} ({} @ {}) ===", entry.dir, entry.module, entry.version);
        match run_entry(entry) {
            Outcome::Ok { table_len, oracle_packages, oracle_decls, unlinked } => {
                eprintln!(
                    "OK  {}: table_len={table_len} oracle_packages={oracle_packages} \
                     oracle_decls={oracle_decls} unlinked_refs={unlinked}",
                    entry.dir
                );
                // Real-content floor, not a bare non-zero check: the finished
                // table must hold at least as many live entries as the oracle
                // independently reported package-level decls (the table also
                // nests fields/methods/params/variants under those, so it can
                // only be >= — anything less means lowering silently dropped
                // declared symbols).
                assert!(
                    table_len >= oracle_decls,
                    "{}: table has fewer live entries ({table_len}) than the oracle's own \
                     independently-counted decls ({oracle_decls}) — lowering dropped symbols",
                    entry.dir
                );
                assert!(
                    oracle_packages > 0,
                    "{}: oracle reported zero packages — not real content",
                    entry.dir
                );
                successes.push((entry.dir, table_len));
            }
            Outcome::Fail { stage, chain } => {
                eprintln!("FAIL {} at [{stage}]: {chain}", entry.dir);
                failures.push((entry.dir, stage, chain));
            }
        }
    }

    eprintln!(
        "\n=== go corpus sweep summary: {}/{} succeeded ===",
        successes.len(),
        ENTRIES.len()
    );
    for (dir, len) in &successes {
        eprintln!("  OK   {dir}: {len} live entries");
    }
    for (dir, stage, chain) in &failures {
        eprintln!("  FAIL {dir} [{stage}]: {chain}");
    }

    assert!(
        failures.is_empty(),
        "{} of {} go corpus entries failed to lower through the real producer; see stderr \
         above for the full error chain of each",
        failures.len(),
        ENTRIES.len()
    );
}
