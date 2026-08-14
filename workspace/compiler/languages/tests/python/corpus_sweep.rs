//! Sweeps every provisioned `pypi` corpus package through the full
//! `nudox_languages::produce` pipeline (`invoke` -> `lower` -> `finish` ->
//! yield-contract gate -> `seal`) against real, third-party sdists under
//! `result/`.
//!
//! # History: this file used to assert the opposite of what it asserts now
//!
//! Before the ruff syntactic front end (`src/syntax.rs`) existed, every one
//! of these 22 entries hit `PythonProducer`'s degraded stub path: `invoke`
//! ignored `PackageSource` entirely, `yield_contract` declared
//! `YieldContract::RootOnly`, and this file's job was to prove that
//! degradation was *declared*, not silently swallowed. `table_len == 1` (the
//! synthesized root, nothing else) was the expected, asserted outcome for
//! all 22 entries — see git history for that version if you need it.
//!
//! Per docs/AGENTS-DOCTRINE.md §4 ("a test that would pass against a stub is not
//! a test") and the mission that added `syntax.rs`, that shape is gone.
//! `PythonProducer` no longer overrides `yield_contract` at all — it takes
//! the trait default (`YieldContract::Declarations`) — so a run that
//! contributes nothing now genuinely fails `produce()` rather than being
//! reported as a documented no-op. This file now asserts what actually
//! matters for a producer that claims to work: per-package **named-symbol
//! canaries** (a real, human-verified export that must appear by name) plus
//! a **floor** on the non-module entry count, both derived from an actual
//! measured run and set safely below it — never a bare count, per
//! docs/AGENTS-DOCTRINE.md §4's "a count-only assertion is satisfiable by a table
//! of module entries named after files."
//!
//! Every entry is measured via `heart::cost::measured` per
//! docs/AGENTS-DOCTRINE.md §4.

use std::path::{Path, PathBuf};

use nudox_ir::change::{EcosystemId, PackageLineageId, PackageName};
use nudox_languages::{
    PackageSource, Producer, ProducerError, ProducerId, YieldContract, produce,
};
use nudox_languages::python::{PythonProducer, oracle::PythonOracle};

/// One corpus entry: `result/<dir>`, the PyPI project name, the
/// ecosystem version string, and its canary set.
///
/// Mirrors `nix/corpus.nix`'s 20 `pypi` `[[packages]]` blocks exactly:
/// 20 packages, of which `click` and `pydantic` each carry two versions for
/// lineage testing, giving **22** version entries.
///
/// # Canaries
///
/// Every name in `canaries` was verified against this exact checkout's
/// source before being added here (see the per-entry comment): each is a
/// real, non-private, non-module symbol the package actually defines at a
/// syntactically visible (not control-flow-gated) position. A canary that
/// does not hold is a bug in this test, not in the producer — per
/// docs/AGENTS-DOCTRINE.md §4.
///
/// `non_module_floor` is a floor, not an expectation: it is set safely below
/// the entry count actually observed on the checkout in this repo (roughly
/// 75-80% of it), so the assertion tolerates small, legitimate shifts (a
/// future ruff version parsing one more file, a corpus re-provision) without
/// being satisfiable by a near-empty table. "Non-module" means every sealed
/// entry whose `KindDiscriminant` is not `Module` — classes, functions,
/// fields, params, consts, aliases, variants, everything a real syntactic
/// walk of the package contributes beyond the one entry per source file.
struct Entry {
    dir: &'static str,
    name: &'static str,
    version: &'static str,
    /// Real, syntactically-visible exports, verified against source.
    canaries: &'static [&'static str],
    /// Floor on sealed non-module entries; see the struct doc above.
    non_module_floor: usize,
}

const ENTRIES: &[Entry] = &[
    // requests/api.py: `get`/`post`/`put`/`delete` free functions.
    // requests/models.py: `Request`, `PreparedRequest`, `Response`.
    // requests/sessions.py: `Session`. Observed non_module=639.
    Entry {
        dir: "requests-2.31.0",
        name: "requests",
        version: "2.31.0",
        canaries: &["get", "post", "put", "delete", "Session", "Request", "Response", "PreparedRequest"],
        non_module_floor: 500,
    },
    // click/core.py: `Command`, `Group`, `Context`, `Option`, `Argument`.
    // click/termui.py/utils.py: `echo`, `prompt`, `confirm`. Observed 1839.
    Entry {
        dir: "click-8.0.4",
        name: "click",
        version: "8.0.4",
        canaries: &["Command", "Group", "Context", "Option", "Argument", "echo", "prompt", "confirm"],
        non_module_floor: 1400,
    },
    Entry {
        dir: "click-8.1.7",
        name: "click",
        version: "8.1.7",
        canaries: &["Command", "Group", "Context", "Option", "Argument", "echo", "prompt", "confirm"],
        non_module_floor: 1400,
    },
    // pydantic 1.x: pydantic/main.py `BaseModel`; pydantic/fields.py
    // `Field`; pydantic/class_validators.py `validator`; pydantic/error_wrappers.py
    // `ValidationError`. Observed 2655.
    Entry {
        dir: "pydantic-1.10.14",
        name: "pydantic",
        version: "1.10.14",
        canaries: &["BaseModel", "Field", "validator", "ValidationError"],
        non_module_floor: 2000,
    },
    // pydantic 2.x renamed the validator decorator to `field_validator`.
    // Observed 6356.
    Entry {
        dir: "pydantic-2.6.1",
        name: "pydantic",
        version: "2.6.1",
        canaries: &["BaseModel", "Field", "field_validator", "ValidationError"],
        non_module_floor: 5000,
    },
    // flask/app.py `Flask`; flask/blueprints.py `Blueprint`; flask/wrappers.py
    // `Request`/`Response`; flask/templating.py `render_template`. Observed 1350.
    Entry {
        dir: "flask-3.0.2",
        name: "flask",
        version: "3.0.2",
        canaries: &["Flask", "Blueprint", "Request", "Response", "render_template"],
        non_module_floor: 1000,
    },
    // attr/_next_gen.py `define`/`field`; attr/_make.py `Factory`; attr/_funcs.py
    // and attr/__init__.py re-export `attrs`/`attrib` as their own defs
    // (`attr/_make.py` defines the legacy `attrib` function; `attrs` is the
    // lowercase legacy alias for `attr.s`). Observed 614.
    Entry {
        dir: "attrs-23.2.0",
        name: "attrs",
        version: "23.2.0",
        canaries: &["define", "field", "Factory", "attrs", "attrib"],
        non_module_floor: 450,
    },
    // sqlalchemy/sql/schema.py `Column`/`Table`; sqlalchemy/engine/create.py
    // `create_engine`; sqlalchemy/orm/relationships.py `relationship`;
    // sqlalchemy/orm/session.py `Session`. Observed 31809 (by far the
    // largest package in the corpus).
    Entry {
        dir: "sqlalchemy-2.0.27",
        name: "sqlalchemy",
        version: "2.0.27",
        canaries: &["Column", "Table", "create_engine", "relationship", "Session"],
        non_module_floor: 25000,
    },
    // yaml/__init__.py `safe_load`/`dump`; yaml/loader.py `Loader`;
    // yaml/dumper.py `Dumper`; yaml/error.py `YAMLError`. Observed 1012.
    Entry {
        dir: "pyyaml-6.0.1",
        name: "pyyaml",
        version: "6.0.1",
        canaries: &["safe_load", "dump", "Loader", "Dumper", "YAMLError"],
        non_module_floor: 750,
    },
    // dateutil/relativedelta.py `relativedelta`; dateutil/rrule.py `rrule`;
    // dateutil/parser/_parser.py `parse`; dateutil/tz/tz.py `tzutc`. Observed 806.
    Entry {
        dir: "python-dateutil-2.8.2",
        name: "python-dateutil",
        version: "2.8.2",
        canaries: &["relativedelta", "rrule", "parse", "tzutc"],
        non_module_floor: 600,
    },
    // six.py: `PY2`/`PY3` are unconditional module-level constants;
    // `with_metaclass`/`add_metaclass`/`ensure_str` are unconditional
    // top-level `def`s. (`string_types` and `iteritems` are real six.py
    // exports too, but both are bound inside `if PY3: ... else: ...` blocks
    // — this producer deliberately does not walk control flow, see
    // `syntax.rs`'s module doc — so they are NOT used as canaries here; using
    // them would assert a capability this front end honestly does not have.)
    // Observed 127.
    Entry {
        dir: "six-1.16.0",
        name: "six",
        version: "1.16.0",
        canaries: &["PY2", "PY3", "with_metaclass", "add_metaclass", "ensure_str"],
        non_module_floor: 90,
    },
    // rich/console.py `Console`; rich/table.py `Table`; rich/panel.py `Panel`;
    // rich/text.py `Text`; rich/__init__.py `print`. Observed 3668.
    Entry {
        dir: "rich-13.7.0",
        name: "rich",
        version: "13.7.0",
        canaries: &["Console", "Table", "Panel", "Text", "print"],
        non_module_floor: 2800,
    },
    // typer/main.py `Typer`; typer/params.py `Argument`/`Option`;
    // typer/main.py `run`. Observed 1122.
    Entry {
        dir: "typer-0.9.0",
        name: "typer",
        version: "0.9.0",
        canaries: &["Typer", "Argument", "Option", "run"],
        non_module_floor: 800,
    },
    // httpx/_client.py `Client`/`AsyncClient`; httpx/_api.py `get`/`post`;
    // httpx/_models.py `Response`. Observed 1943.
    Entry {
        dir: "httpx-0.27.0",
        name: "httpx",
        version: "0.27.0",
        canaries: &["Client", "AsyncClient", "get", "post", "Response"],
        non_module_floor: 1500,
    },
    // black/mode.py `Mode`/`FileMode`; black/__init__.py
    // `format_str`/`format_file_contents`. Observed 2472.
    Entry {
        dir: "black-24.2.0",
        name: "black",
        version: "24.2.0",
        canaries: &["Mode", "FileMode", "format_str", "format_file_contents"],
        non_module_floor: 1900,
    },
    // more_itertools/more.py `chunked`/`first`; more_itertools/recipes.py
    // `flatten`/`unique_everseen`. (`pairwise` is a real export too, but is
    // bound inside a `try: ... except ImportError: ... else: ...` at module
    // scope — the same documented control-flow gap as six.py above — so it
    // is deliberately not used as a canary.) Observed 591.
    Entry {
        dir: "more-itertools-10.2.0",
        name: "more-itertools",
        version: "10.2.0",
        canaries: &["chunked", "flatten", "first", "unique_everseen"],
        non_module_floor: 450,
    },
    // tenacity/__init__.py `retry`/`Retrying`; tenacity/stop.py
    // `stop_after_attempt`; tenacity/wait.py `wait_fixed`. Observed 422.
    Entry {
        dir: "tenacity-8.2.3",
        name: "tenacity",
        version: "8.2.3",
        canaries: &["retry", "Retrying", "stop_after_attempt", "wait_fixed"],
        non_module_floor: 300,
    },
    // dataclasses_json/api.py `DataClassJsonMixin`; dataclasses_json/core.py
    // and cfg.py `dataclass_json`/`LetterCase`. Observed 294.
    Entry {
        dir: "dataclasses-json-0.6.4",
        name: "dataclasses-json",
        version: "0.6.4",
        canaries: &["DataClassJsonMixin", "dataclass_json", "LetterCase"],
        non_module_floor: 200,
    },
    // structlog/_config.py `get_logger`; structlog/stdlib.py `BoundLogger`;
    // structlog/_output.py `PrintLogger`. Observed 1284.
    Entry {
        dir: "structlog-24.1.0",
        name: "structlog",
        version: "24.1.0",
        canaries: &["get_logger", "BoundLogger", "PrintLogger"],
        non_module_floor: 950,
    },
    // jsonschema/validators.py `validate`/`Draft7Validator`;
    // jsonschema/exceptions.py `ValidationError`; jsonschema/_format.py
    // `FormatChecker`. Observed 709.
    Entry {
        dir: "jsonschema-4.21.1",
        name: "jsonschema",
        version: "4.21.1",
        canaries: &["validate", "Draft7Validator", "ValidationError", "FormatChecker"],
        non_module_floor: 500,
    },
    // cattr/converters.py `Converter`/`GenConverter`;
    // cattr/__init__.py `structure`/`unstructure`. Observed 683.
    Entry {
        dir: "cattrs-23.2.3",
        name: "cattrs",
        version: "23.2.3",
        canaries: &["Converter", "structure", "unstructure", "GenConverter"],
        non_module_floor: 500,
    },
    // bs4/__init__.py `BeautifulSoup`; bs4/element.py `Tag`/`NavigableString`.
    // Observed 999.
    Entry {
        dir: "beautifulsoup4-4.12.3",
        name: "beautifulsoup4",
        version: "4.12.3",
        canaries: &["BeautifulSoup", "Tag", "NavigableString"],
        non_module_floor: 750,
    },
];

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../result")
        .canonicalize()
        .expect("no result/ checkout — see docs/CORPUS.md to (re)provision it")
}

/// Walk the full `std::error::Error` source chain. The top-level `Display`
/// on `ProducerError` is deliberately terse (docs/AGENTS-DOCTRINE.md's "hard-won
/// facts": reading only it turns a five-second diagnosis into an hour).
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
    Produced {
        table_len: usize,
        non_module: usize,
        missing_canaries: Vec<&'static str>,
        contract_is_declarations: bool,
    },
    Preflight {
        reason: String,
    },
    Fail {
        stage: &'static str,
        chain: String,
    },
}

fn run_entry(entry: &Entry) -> Outcome {
    let root = corpus_root().join(entry.dir);
    let has_setup_py = root.join("setup.py").is_file();
    let has_pyproject = root.join("pyproject.toml").is_file();
    if !has_setup_py && !has_pyproject {
        return Outcome::Preflight {
            reason: format!(
                "no setup.py or pyproject.toml at {} — not a genuine sdist",
                root.display()
            ),
        };
    }

    let src = PackageSource::new(&root, entry.name, entry.version);
    let lid = PackageLineageId::new(EcosystemId::new("pypi"), PackageName::new(entry.name));

    let case = format!("pypi-sweep-{}", entry.dir);
    let (produced, _cost) = heart::cost::measured(&case, &root, || {
        produce(&PythonProducer, &src, &lid, &nudox_ir::foreign::Unlinked)
    });

    match produced {
        Ok(p) => {
            let names: std::collections::HashSet<&str> = p
                .table
                .iter()
                .filter_map(|(_, e)| {
                    let n = e.sym().name.as_str();
                    (!n.is_empty()).then_some(n)
                })
                .collect();
            let missing_canaries: Vec<&'static str> =
                entry.canaries.iter().copied().filter(|c| !names.contains(c)).collect();
            let non_module = p
                .table
                .iter()
                .filter(|(_, e)| e.kind().discriminant().map(|d| format!("{d:?}")) != Some("Module".to_owned()))
                .count();
            Outcome::Produced {
                table_len: p.table.len(),
                non_module,
                missing_canaries,
                contract_is_declarations: matches!(p.contract, YieldContract::Declarations),
            }
        }
        Err(e) => Outcome::Fail {
            stage: "produce (invoke/lower/finish/yield-contract/seal)",
            chain: chain(&e),
        },
    }
}

#[test]
fn every_provisioned_pypi_corpus_package_lowers_real_named_declarations() {
    let mut preflight_failures = Vec::new();
    let mut failures = Vec::new();
    let mut canary_failures: Vec<(&str, Vec<&str>)> = Vec::new();
    let mut floor_failures: Vec<(&str, usize, usize)> = Vec::new();
    let mut contract_failures: Vec<&str> = Vec::new();
    let mut produced_summary: Vec<(&str, usize, usize)> = Vec::new();

    for entry in ENTRIES {
        eprintln!("=== {} ({} @ {}) ===", entry.dir, entry.name, entry.version);
        match run_entry(entry) {
            Outcome::Produced { table_len, non_module, missing_canaries, contract_is_declarations } => {
                eprintln!(
                    "PRODUCED {}: table_len={table_len} non_module={non_module} \
                     contract_is_declarations={contract_is_declarations} missing_canaries={missing_canaries:?}",
                    entry.dir
                );
                if !contract_is_declarations {
                    contract_failures.push(entry.dir);
                }
                if !missing_canaries.is_empty() {
                    canary_failures.push((entry.dir, missing_canaries));
                }
                if non_module < entry.non_module_floor {
                    floor_failures.push((entry.dir, non_module, entry.non_module_floor));
                }
                produced_summary.push((entry.dir, table_len, non_module));
            }
            Outcome::Preflight { reason } => {
                eprintln!("PREFLIGHT-FAIL {}: {reason}", entry.dir);
                preflight_failures.push((entry.dir, reason));
            }
            Outcome::Fail { stage, chain } => {
                eprintln!("FAIL {} at [{stage}]: {chain}", entry.dir);
                failures.push((entry.dir, stage, chain));
            }
        }
    }

    eprintln!("\n=== pypi corpus sweep summary: {} of {} entries produced ===", produced_summary.len(), ENTRIES.len());
    for (dir, table_len, non_module) in &produced_summary {
        eprintln!("  {dir}: table_len={table_len} non_module={non_module}");
    }

    assert!(
        preflight_failures.is_empty(),
        "{} of {} pypi corpus entries are not genuine sdists on disk: {preflight_failures:?}",
        preflight_failures.len(),
        ENTRIES.len(),
    );
    assert!(
        failures.is_empty(),
        "{} of {} pypi corpus entries failed to lower through produce(); see stderr above \
         for the full error chain of each: {failures:?}",
        failures.len(),
        ENTRIES.len(),
    );
    assert!(
        contract_failures.is_empty(),
        "these entries did not report YieldContract::Declarations (the trait default, since \
         PythonProducer no longer overrides yield_contract): {contract_failures:?}",
    );
    assert!(
        canary_failures.is_empty(),
        "these entries are missing at least one verified real symbol — either a genuine \
         regression in the producer, or the corpus checkout no longer matches the source this \
         test's canaries were read from: {canary_failures:?}",
    );
    assert!(
        floor_failures.is_empty(),
        "these entries fell below their non-module entry floor (dir, observed, floor): {floor_failures:?}",
    );
}

/// The yield-contract gate is live, not vacuous: a producer that genuinely
/// contributes nothing — while still taking the trait default
/// `YieldContract::Declarations`, exactly like `PythonProducer` itself now
/// does — is rejected by `produce()`, and the real `PythonProducer` on the
/// identical package is accepted because it actually declares something.
///
/// This is the repurposed twin of the pre-`syntax.rs` version of this test
/// (`the_declaration_is_what_keeps_python_out_of_the_error_path`), updated
/// for docs/AGENTS-DOCTRINE.md §8's "verify the guard by mutation" now that the
/// thing being guarded against is different: it used to be "an undeclared
/// stub is wrongly accepted"; it is now "a producer that silently regresses
/// to contributing nothing is wrongly accepted". `EmptyPython` delegates
/// nothing to the real producer — its `invoke`/`lower` are hand-written
/// no-ops — so this measures the gate, not `PythonProducer`'s own behavior.
struct EmptyPython;

impl Producer for EmptyPython {
    type Id = <PythonProducer as Producer>::Id;
    type Oracle = PythonOracle;

    const ID: ProducerId = ProducerId("python-empty-mutant/1");
    const LANGUAGE: nudox_ir::body::Language = <PythonProducer as Producer>::LANGUAGE;

    fn invoke(&self, _src: &PackageSource) -> Result<Self::Oracle, ProducerError> {
        Ok(PythonOracle::default())
    }

    // No `yield_contract` override — the trait default (`Declarations`)
    // applies, exactly as it now does for the real `PythonProducer`.

    fn lower(
        &self,
        _oracle: &Self::Oracle,
        _out: &mut nudox_ir::lower::Lowering<Self::Id>,
    ) -> Result<(), ProducerError> {
        Ok(())
    }
}

#[test]
fn a_producer_that_contributes_nothing_is_rejected_even_under_the_default_contract() {
    let entry = &ENTRIES[0];
    let root = corpus_root().join(entry.dir);
    let src = PackageSource::new(&root, entry.name, entry.version);
    let lid = PackageLineageId::new(EcosystemId::new("pypi"), PackageName::new(entry.name));

    let case = format!("pypi-empty-mutant-{}", entry.dir);
    let (result, _cost) = heart::cost::measured(&case, &root, || {
        produce(&EmptyPython, &src, &lid, &nudox_ir::foreign::Unlinked)
    });

    match result {
        Err(ProducerError::NoDeclarationsContributed { package, producer, source }) => {
            eprintln!("mutant rejected as expected: package={package} producer={producer} source={source:?}");
            assert_eq!(package, entry.name);
            assert_eq!(producer, EmptyPython::ID);
        }
        Err(other) => panic!(
            "expected ProducerError::NoDeclarationsContributed for a producer that contributes \
             nothing and declares no degradation, got {other:?}"
        ),
        Ok(p) => panic!(
            "the yield-contract gate did not fire: a producer that reads no source sealed a \
             table of {} entries and was reported as a success",
            p.table.len()
        ),
    }

    // And the real producer, on the identical input, is accepted — because
    // it actually declares real content, not because the gate is asleep.
    let honest = produce(&PythonProducer, &src, &lid, &nudox_ir::foreign::Unlinked)
        .expect("the real producer must be accepted: it genuinely reads and lowers `src`");
    assert!(
        matches!(honest.contract, YieldContract::Declarations),
        "and it is accepted under the real (non-degraded) contract"
    );
    assert!(honest.table.len() > 1, "and with real content beyond the synthesized root");
}
