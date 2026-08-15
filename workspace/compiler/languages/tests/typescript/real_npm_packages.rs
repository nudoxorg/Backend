//! Lower every real npm-ecosystem fixture in `nix/corpus.nix` end to
//! end through the actual `Producer` trait (entry discovery -> OXC extraction
//! -> `Lowering` -> seal), and assert on real content.
//!
//! # Why this exists
//!
//! Every other test in this crate (`producer_tests.rs`) parses a literal
//! TypeScript string written by hand. docs/AGENTS-DOCTRINE.md §4 is explicit about
//! what that measures: the fixture author's imagination, not the producer's
//! behaviour on a package nobody here wrote. The Go producer's 47
//! fixture-only tests were 100% green and 0% usable on the first real
//! package it saw. This file is the counterpart: it drives the same
//! `nudox_languages::produce` entry point the store/engine use in production,
//! over the 20 real npm tarballs `nix build .#checks.corpus` materializes into
//! `result/`, and asserts on entries that must be present in each
//! package's real public API — never just `is_ok()`.
//!
//! # Fixture list
//!
//! Mirrors the `ecosystem = "npm"` section of `nix/corpus.nix` exactly
//! (20 packages, 22 version entries — `lodash` and `zod` each carry two for
//! lineage testing). Kept as a literal list rather than parsed from the TOML
//! at test time so a missing/renamed fixture fails as a clear per-case SKIP
//! instead of a parse error that takes the whole file down with it. If the
//! manifest's npm section changes, update this list to match.
//!
//! # Running
//!
//! ```text
//! cargo test -p nudox-languages --test typescript_real_npm_packages -- --nocapture
//! ```
//!
//! Each case prints a `cost case=…` line (docs/AGENTS-DOCTRINE.md §4) and, on
//! success, an entry count. A missing checkout is a visible per-case SKIP
//! (an environment problem, not a code defect) rather than a failure — run
//! `nix build .#checks.corpus` first to materialize the corpus.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use nudox_ir::change::{EcosystemId, PackageLineageId, PackageName};
use nudox_ir::entry::{EntryInner, Visibility};
use nudox_ir::foreign::Unlinked;
use nudox_ir::kind::Kind;
use nudox_languages::{PackageSource, YieldContract, produce};
use nudox_languages::typescript::TypescriptProducer;

/// One npm corpus fixture: the manifest package name, the pinned version, the
/// `result/` directory name `fetch.nu`'s `safe-dir-name` produces for it
/// (`/` -> `__`, joined with `-<version>`), and what the producer owes it.
///
/// # Why `floor` + `must_contain`, and not the old `Expect` enum
///
/// This sweep used to carry an `Expect::Stub { contributed, why }` case for
/// four packages (`lodash` at both pinned versions, `debug`, `ws`) that
/// pinned a degenerate 1- or 2-entry result as an *acceptable, exact* count,
/// on the theory that these CommonJS packages had no top-level declaration
/// for the extractor to read at all. That theory does not survive contact
/// with the checkouts: `ws` ships a real ESM entry (`wrapper.mjs`) with five
/// named exports, and `lodash`'s ~300-function API lives in 331 sibling
/// `.js` files next to the UMD bundle `main` points at — each with its own
/// real top-level declaration. The bug was in the extractor (no CommonJS
/// `module.exports`/`exports.X` recognition, and no "no `exports` field
/// means every file is importable" fallback), not in the packages, and
/// `Expect::Stub` encoded the bug's output as policy.
///
/// It was also unfalsifiable by construction: `Stub`'s `contributed` count
/// was checked with no regard for *which* names, if any, were public and
/// real — a table holding only the entry-file's own module name (`"lodash"`,
/// `"index"`) passed identically to a table holding real content, because
/// the sweep's only content check (`identifiers != 0`) is satisfied by the
/// module entry's own name, which is always a real identifier.
///
/// The replacement is two independent claims per fixture:
/// - `floor`: at least this many declarations, counted the same way as
///   before (contributed beyond the synthesized root). Set with margin below
///   the count measured on 2026-08-08 so ordinary extractor churn does not
///   trip it — not the exact measured number, which would make this a
///   change-detector instead of an assertion.
/// - `must_contain`: real, source-verified symbol names that must appear
///   among this fixture's `Visibility::Public`, **non-module** contributed
///   entries. This is the check `floor` alone cannot make honest: a package
///   whose only reachable "declarations" are one-per-file module entries
///   (exactly what enumerating a CommonJS package's file tree without also
///   teaching the extractor to read `module.exports` produces) can inflate
///   `floor` arbitrarily while `must_contain` — filtered to
///   non-`EntryInner::Owned(Kind::Module(_))` — stays red, because every
///   name in it would land `Visibility::Private` without real CommonJS
///   export recognition. Empty for fixtures whose triple-digit-plus floor is
///   already a claim no stub could satisfy.
///
/// Every name in every `must_contain` list below was checked against the
/// 2026-08-08 checkout under `result/` before being pinned — see the
/// per-fixture comments for exactly where each symbol is declared and
/// exported. A canary naming a symbol the package does not actually export
/// is a bug in this file, not a floor to be met by inventing one.
struct Fixture {
    name: &'static str,
    version: &'static str,
    dir: &'static str,
    floor: usize,
    must_contain: &'static [&'static str],
}

/// The 22 npm version entries from `nix/corpus.nix`, in manifest order.
const FIXTURES: &[Fixture] = &[
    // `lodash.js` (the `main` entry, a 17k-line UMD bundle) is one
    // `ExpressionStatement`; the real ~300-function API lives in sibling
    // top-level `.js` files (`chunk.js`, `debounce.js`, `merge.js`,
    // `cloneDeep.js`, `isEqual.js`, …), each `function <name>(...) { ... }`
    // followed by `module.exports = <name>;`. `package.json` has no
    // `exports` field, so Node's classic resolution makes every one of
    // those 333 top-level `.js` files real, importable API
    // (`require('lodash/chunk')`) — verified by listing
    // `result/lodash-4.17.21/*.js` directly.
    Fixture { name: "lodash", version: "4.17.21", dir: "lodash-4.17.21",
              floor: 250,
              must_contain: &["chunk", "debounce", "merge", "cloneDeep", "isEqual"] },
    Fixture { name: "lodash", version: "4.17.20", dir: "lodash-4.17.20",
              floor: 250,
              must_contain: &["chunk", "debounce", "merge", "cloneDeep", "isEqual"] },
    Fixture { name: "zod", version: "3.22.4", dir: "zod-3.22.4",
              floor: 800, must_contain: &[] },
    Fixture { name: "zod", version: "3.23.8", dir: "zod-3.23.8",
              floor: 800, must_contain: &[] },
    Fixture { name: "type-fest", version: "4.10.2", dir: "type-fest-4.10.2",
              floor: 300, must_contain: &[] },
    Fixture { name: "chalk", version: "5.3.0", dir: "chalk-5.3.0",
              floor: 80, must_contain: &[] },
    Fixture { name: "commander", version: "12.0.0", dir: "commander-12.0.0",
              floor: 250, must_contain: &[] },
    Fixture { name: "axios", version: "1.6.7", dir: "axios-1.6.7",
              floor: 400, must_contain: &[] },
    Fixture { name: "date-fns", version: "3.3.1", dir: "date-fns-3.3.1",
              floor: 1500, must_contain: &[] },
    Fixture { name: "rxjs", version: "7.8.1", dir: "rxjs-7.8.1",
              floor: 1500, must_contain: &[] },
    Fixture { name: "immer", version: "10.0.3", dir: "immer-10.0.3",
              floor: 40, must_contain: &[] },
    Fixture { name: "uuid", version: "9.0.1", dir: "uuid-9.0.1",
              floor: 30, must_contain: &[] },
    // `main` is `./src/index.js`, which just re-dispatches to `browser.js`
    // or `node.js` at runtime (`if (...) { module.exports = require(...) }`
    // — a single `IfStatement`). `package.json` has no `exports` field, so
    // all four `.js` files under `src/` (`index.js`, `browser.js`,
    // `node.js`, `common.js`) are enumerated as declaration roots directly.
    // `node.js` and `browser.js` each declare top-level `function
    // useColors()`, `function formatArgs(...)`, `function save(...)`,
    // `function load(...)`, and then `exports.useColors = useColors;`, etc.
    // — real top-level `exports.X = <ident>` CommonJS exports.
    //
    // NOT included: `enable`/`disable`/`formatters`, which the original
    // diagnosis for this fixture proposed as canaries. Verified false:
    // `enable`/`disable`/`enabled`/`coerce`/`destroy` are declared *inside*
    // `common.js`'s `function setup(env) { ... }`, assigned only to a local
    // `createDebug` variable's properties, never at module top level in any
    // of the four files. `formatters` is reached only via `const
    // {formatters} = module.exports;` followed by `formatters.o = ...` — a
    // destructured local, not `module.exports.X` or the module's own export
    // binding. Recovering any of these needs interprocedural analysis of a
    // nested function's return value, which is out of scope here and belongs
    // to a documented gap, not a canary this sweep pins.
    Fixture { name: "debug", version: "4.3.4", dir: "debug-4.3.4",
              floor: 15,
              must_contain: &["useColors", "formatArgs", "save", "load"] },
    Fixture { name: "left-pad", version: "1.3.0", dir: "left-pad-1.3.0",
              floor: 4, must_contain: &[] },
    Fixture { name: "yup", version: "1.4.0", dir: "yup-1.4.0",
              floor: 250, must_contain: &[] },
    // `exports["."]["import"]` points at `wrapper.mjs`, real ESM with five
    // named exports re-exported from CommonJS siblings under `lib/`:
    // `import WebSocket from './lib/websocket.js'; ... export {
    // createWebSocketStream, Receiver, Sender, WebSocket, WebSocketServer
    // };`. Each `lib/*.js` file declares its class/function at module top
    // level (`class WebSocket extends EventEmitter { ... }`,
    // `module.exports = WebSocket;`) — verified in
    // `result/ws-8.16.0/wrapper.mjs` and `lib/{websocket,receiver,
    // sender,stream,websocket-server}.js`.
    Fixture { name: "ws", version: "8.16.0", dir: "ws-8.16.0",
              floor: 5,
              must_contain: &["WebSocket", "WebSocketServer", "Receiver", "Sender",
                               "createWebSocketStream"] },
    Fixture { name: "@types/node", version: "20.11.0", dir: "@types__node-20.11.0",
              floor: 9000, must_contain: &[] },
    Fixture { name: "fp-ts", version: "2.16.5", dir: "fp-ts-2.16.5",
              floor: 5000, must_contain: &[] },
    Fixture { name: "class-validator", version: "0.14.1", dir: "class-validator-0.14.1",
              floor: 800, must_contain: &[] },
    Fixture { name: "reflect-metadata", version: "0.2.1", dir: "reflect-metadata-0.2.1",
              floor: 40, must_contain: &[] },
    Fixture { name: "p-limit", version: "5.0.0", dir: "p-limit-5.0.0",
              floor: 3, must_contain: &[] },
    Fixture { name: "dayjs", version: "1.11.10", dir: "dayjs-1.11.10",
              floor: 80, must_contain: &[] },
];

/// Root of the corpus checkout directory, resolved relative to this crate's
/// manifest so the test works regardless of the invoking shell's cwd.
fn corpus_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../result")
}

fn fixture_root(dir: &str) -> PathBuf {
    corpus_root().join(dir)
}

/// Whether `name` is a real, non-empty JavaScript/TypeScript identifier rather
/// than a path fragment, a file name, or a placeholder.
///
/// The doc comment on the sweep below has promised this check since the file
/// was written; it is implemented here rather than as `!name.contains('/')`
/// because "no path separator" is the weakest possible reading of it. A leading
/// alphabetic/`_`/`$` followed only by identifier characters excludes `/` and
/// `\` by construction, and also excludes the shapes a stubbed extractor
/// actually emits — `""`, `"index.d.ts"`, `"src/index"`, `"<anonymous>"`,
/// `"0"` — none of which `!contains('/')` would have caught.
fn is_real_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_alphabetic() || first == '_' || first == '$' => {
            chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        }
        _ => false,
    }
}

/// Whether `entry` is the kind of thing `must_contain` is entitled to demand:
/// a real, publicly-visible declaration, not the synthetic per-file `Module`
/// entry this producer declares for every module it walks.
///
/// Without the `Module` exclusion, enumerating a CommonJS package's file tree
/// (declaration-root discovery, see `Fixture`'s doc comment) alone would let
/// `lodash/chunk.js` satisfy a `"chunk"` canary just by *existing* as a
/// module named `chunk` — the module entry's own name coincides with the
/// function it contains, because lodash names each file after the one thing
/// it exports. That would pass identically whether or not the extractor can
/// actually read `module.exports = chunk;`. Filtering to non-`Module` kinds
/// is what keeps the canary honest: it can only be satisfied by a `Function`/
/// `Record`/etc. entry the extractor produced from real `module.exports`
/// recognition, not by file-tree enumeration alone.
fn is_public_non_module(entry: &nudox_ir::entry::Entry) -> bool {
    entry.sym().visibility == Visibility::Public
        && !matches!(entry.kind(), EntryInner::Owned(Kind::Module(_)))
}

/// What one fixture's lowering actually yielded, in the terms the sweep asserts
/// on.
struct Lowered {
    /// Declarations the producer contributed **beyond** the root `produce`
    /// synthesizes before `lower` is ever called.
    ///
    /// Derived from the entries themselves — an entry the producer declared has
    /// a parent, and `seal` leaves exactly one parentless entry, the
    /// synthesized root (`ir/model/src/package/seal.rs`'s
    /// `seal_materializes_the_tree` pins that) — never as `len() - 1`, which
    /// would be arithmetic over a representation this test does not own.
    contributed: usize,
    /// How many contributed entries carry a real identifier name.
    identifiers: usize,
    /// Names of contributed entries that are both `Visibility::Public` and
    /// not a `Module` entry — what `must_contain` is checked against.
    public_non_module_names: HashSet<String>,
    /// The first few contributed names, so a failure says *what* was found
    /// instead of only that the count was wrong.
    sample: Vec<String>,
    /// The contract `produce` held the producer to for this package.
    contract: YieldContract,
}

/// Lower one fixture through the real `Producer` pipeline, or return the full
/// error chain as `Err`.
///
/// Walks `std::error::Error::source` per docs/AGENTS-DOCTRINE.md §8 ("print the
/// whole `#[source]` chain") — this crate's `ProducerError` wraps
/// `discover_entry_points`/`build_and_extract` failures behind
/// `ProducerError::OracleSpawn { reason: io::Error::other(e), .. }`, and the
/// terse top-level `Display` alone ("workspace load failed"-style) does not
/// name the file or the real cause.
fn lower(root: &Path, name: &str, version: &str) -> Result<Lowered, String> {
    let src = PackageSource::new(root, name, version);
    let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new(name));

    produce(&TypescriptProducer::new(), &src, &lineage, &Unlinked)
        .map(|produced| {
            let table = &produced.table;
            let declared: Vec<&nudox_ir::entry::Entry> = table
                .iter()
                .filter(|(id, _)| table.parent_of(*id).is_some())
                .map(|(_, e)| e)
                .collect();

            Lowered {
                contributed: declared.len(),
                identifiers: declared
                    .iter()
                    .filter(|e| is_real_identifier(&e.sym().name))
                    .count(),
                public_non_module_names: declared
                    .iter()
                    .filter(|e| is_public_non_module(e))
                    .map(|e| e.sym().name.clone())
                    .collect(),
                sample: declared.iter().take(6).map(|e| e.sym().name.clone()).collect(),
                contract: produced.contract.clone(),
            }
        })
        .map_err(|err| {
            let mut chain = format!("{err}");
            let mut cursor: &dyn std::error::Error = &err;
            while let Some(source) = std::error::Error::source(cursor) {
                let _ = write!(chain, "\n  caused by: {source}");
                cursor = source;
            }
            chain
        })
}

/// Every present npm fixture lowers to a non-trivial, well-formed IR table.
///
/// One test, not 22, so a single `cargo test` run reports the whole corpus
/// sweep in one line-per-case log instead of 22 separate harness entries —
/// but every case is measured individually via `heart::cost::measured`
/// so the `cost case=…` line (and thus the per-package number) survives.
///
/// Assertions per fixture (never `is_ok()`, per §4):
/// * the producer's [`YieldContract`] is not a declared degradation — an
///   inert producer must not be counted here as a documented package;
/// * the count of declarations it contributed *beyond the synthesized root*
///   meets the fixture's `floor`;
/// * at least one contributed entry's name is a real identifier
///   (`is_real_identifier`) — a baseline sanity check kept from the original
///   version of this sweep;
/// * every name in `must_contain` appears among the fixture's `Visibility::
///   Public`, non-`Module` contributed entries (`is_public_non_module`) — the
///   check that actually distinguishes real extraction from file-tree
///   enumeration; see `Fixture`'s doc comment.
///
/// # What the count used to be, and why it could not fail
///
/// This sweep previously failed only on `entry_count == 0`, taken from
/// `produced.table.len()`. That branch was unreachable: [`produce`] builds the
/// root [`nudox_ir::entry::Symbol`] itself and hands it to `Lowering::new`
/// before `lower` is called, so *every* successful run seals a table of at
/// least one entry. A producer that read no bytes at all scored 1 and passed.
/// A later revision replaced that with a floor plus `identifiers != 0`, and
/// then papered over four still-degenerate packages with an
/// `Expect::Stub { contributed, why }` case that asserted their broken output
/// as policy (see `Fixture`'s doc comment for the full history). This version
/// is the one that actually demands real extraction from those four.
///
/// Any fixture whose checkout is missing is skipped with a printed reason
/// (environment problem, not a code defect) rather than failing the whole
/// sweep, and the final assertion fails loudly if *none* of the 22 were
/// actually exercised — a directory rename that silently skipped every case
/// must not read as a green run.
#[test]
fn every_real_npm_fixture_contributes_the_declarations_it_is_pinned_to() {
    let mut ran = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for fixture in FIXTURES {
        let root = fixture_root(fixture.dir);
        if !root.join("package.json").is_file() {
            eprintln!(
                "SKIP: no checkout at {} (run `nix build .#checks.corpus` to materialize the corpus)",
                root.display()
            );
            continue;
        }

        // Counted here, not on the success path: `ran` is "fixtures this run
        // actually exercised", and it is the denominator of the summary below.
        // Incrementing it only on `Ok` made a fixture that both lowered and
        // then failed an assertion appear in numerator and denominator
        // separately — the summary read "2/24" over a 22-entry corpus.
        ran += 1;

        let case = format!("lower/npm/{}-{}", fixture.name, fixture.version);
        let (result, cost) = heart::cost::measured(&case, &root, || {
            lower(&root, fixture.name, fixture.version)
        });

        let lowered = match result {
            Ok(lowered) => lowered,
            Err(chain) => {
                failures.push(format!("{}-{}: {chain}", fixture.name, fixture.version));
                continue;
            }
        };

        let Lowered {
            contributed,
            identifiers,
            public_non_module_names,
            sample,
            contract,
        } = lowered;
        eprintln!(
            "OK: {}-{} contributed {contributed} declarations ({identifiers} named by a \
             real identifier, {} public non-module) in {:.2}s; first names: {sample:?}",
            fixture.name,
            fixture.version,
            public_non_module_names.len(),
            cost.wall.as_secs_f64()
        );

        // A producer that declares itself inert must not be tallied as a
        // documented package. `produce` already rejects a *contradiction*
        // between the declaration and the output; what it cannot know is that
        // this corpus expects real analysis, which is this sweep's to say.
        if let Some(degraded) = contract.degraded() {
            failures.push(format!(
                "{}-{}: the producer declares `YieldContract::RootOnly` — this corpus \
                 exists to measure real extraction, not a declared degradation: {degraded}",
                fixture.name, fixture.version
            ));
            continue;
        }

        if contributed < fixture.floor {
            failures.push(format!(
                "{}-{}: contributed {contributed} declarations beyond the synthesized \
                 root, expected at least {} — this is not real extraction. First names: \
                 {sample:?}",
                fixture.name, fixture.version, fixture.floor
            ));
            continue;
        }

        // The content assertion (docs/AGENTS-DOCTRINE.md §4): a count is satisfied by
        // a table of placeholders, a real exported name is not.
        if identifiers == 0 {
            failures.push(format!(
                "{}-{}: none of its {contributed} contributed entries is named by a real \
                 identifier — a table of path fragments or placeholders is not a lowered \
                 public API. First names: {sample:?}",
                fixture.name, fixture.version
            ));
        }

        // The canary (this file's replacement for `Expect::Stub`): named,
        // source-verified public symbols must actually be present as real,
        // non-module declarations — never satisfiable by declaration-root
        // enumeration alone. See `Fixture`'s doc comment and
        // `is_public_non_module`.
        let missing: Vec<&str> = fixture
            .must_contain
            .iter()
            .filter(|name| !public_non_module_names.contains(**name))
            .copied()
            .collect();
        if !missing.is_empty() {
            failures.push(format!(
                "{}-{}: missing {missing:?} among {} public non-module declarations \
                 (verified real, exported symbols — see the fixture's doc comment for where \
                 each is declared in the real package source). First names: {sample:?}",
                fixture.name,
                fixture.version,
                public_non_module_names.len()
            ));
        }
    }

    assert!(
        ran > 0,
        "no npm fixtures were found under {} — run `nix build .#checks.corpus` first",
        corpus_root().display()
    );

    assert!(
        failures.is_empty(),
        "{}/{ran} exercised npm fixtures did not contribute the declarations they are \
         pinned to:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
