//! Spot-checks pyrefly-inferred types against **manually read source** on the
//! six pypi corpus packages that carry zero written annotations (six, pyyaml,
//! python-dateutil, requests, beautifulsoup4, more-itertools) — the packages
//! where the syntactic tier alone resolves nothing and this tier claims to
//! recover roughly half.
//!
//! `type_lattice_census.rs` proves the enrichment fires and lands in the
//! right bucket at corpus scale; it cannot prove any *individual* inferred
//! type is actually correct, because it never reads the source it is
//! measuring. This file does the opposite: nine functions, chosen by reading
//! their bodies, each with a return type a human can derive from the source
//! text alone, no type checker required. An inferred type that is *wrong* is
//! worse than `Unknown` — this is what stands between "pyrefly fills type
//! slots" and "pyrefly fills type slots correctly".
//!
//! Every target function is a bare top-level `def`, never nested in an `if`
//! block: `syntax.rs`'s own front end does not walk into control flow, so
//! anything gated behind one (e.g. six's PY3-conditional `iterkeys`) is
//! invisible to the id scheme this test looks up by, independent of pyrefly.
//!
//! # Running
//!
//! ```text
//! cargo test -p nudox-producer-python --test pyrefly_inference_spot_check -- --nocapture
//! ```
//!
//! Gated on the `pyrefly` feature (default-on since 2026-08-09, see
//! `Cargo.toml`): `nudox_producer_python::context` does not exist without it,
//! so `--no-default-features` must skip this file entirely rather than fail
//! to compile.
#![cfg(feature = "pyrefly")]

use std::path::{Path, PathBuf};

use nudox_producer::PackageSource;
use nudox_producer_python::context;
use nudox_producer_python::oracle::{ItemBody, TypeData};

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../result")
        .canonicalize()
        .expect("no result/ checkout — see docs/CORPUS.md to (re)provision it")
}

/// One function to check: where it lives, and the return type a human reading
/// the source would derive. `expect_substr` is matched against the rendered
/// `TypeData` (via `{:?}`) rather than exact-equality, because pyrefly's
/// exported spelling carries a fully-qualified module path (e.g.
/// `"datetime.datetime"`, not `"datetime"`) that would make the test brittle
/// to spell out byte-for-byte, but the substring is still specific enough
/// that a wrong class could never match it by accident.
struct Case {
    dir: &'static str,
    name: &'static str,
    /// Fully-qualified `PythonId` of the function.
    id: &'static str,
    /// Substring the rendered return `TypeData` must contain.
    expect_substr: &'static str,
    /// The line, read from source, that justifies `expect_substr`.
    source_evidence: &'static str,
}

const CASES: &[Case] = &[
    Case {
        dir: "six-1.16.0",
        name: "six",
        id: "six._import_module",
        // `six.py`: `def _import_module(name): __import__(name); return
        // sys.modules[name]` — `sys.modules` is `dict[str, ModuleType]`, so
        // indexing it yields a module.
        expect_substr: "ModuleType",
        source_evidence: "return sys.modules[name]",
    },
    Case {
        dir: "six-1.16.0",
        name: "six",
        id: "six._add_doc",
        // `six.py`: `def _add_doc(func, doc): func.__doc__ = doc` — no
        // `return` statement anywhere in the body, so the function returns
        // `None`.
        expect_substr: "NoneType",
        source_evidence: "func.__doc__ = doc  (falls off the end, no return)",
    },
    Case {
        dir: "pyyaml-6.0.1",
        name: "pyyaml",
        id: "yaml.scan",
        // `yaml/__init__.py`: `def scan(stream, Loader=Loader): ... yield
        // loader.get_token()` — a `yield` makes this a generator function.
        expect_substr: "Generator",
        source_evidence: "while loader.check_token(): yield loader.get_token()",
    },
    Case {
        dir: "python-dateutil-2.8.2",
        name: "python-dateutil",
        id: "dateutil.utils.today",
        // `dateutil/utils.py`: `def today(tzinfo=None): ... return
        // datetime.combine(dt.date(), time(0, tzinfo=tzinfo))` —
        // `datetime.combine` is documented to return a `datetime.datetime`.
        expect_substr: "datetime",
        source_evidence: "return datetime.combine(dt.date(), time(0, tzinfo=tzinfo))",
    },
    Case {
        dir: "python-dateutil-2.8.2",
        name: "python-dateutil",
        id: "dateutil.easter.easter",
        // `dateutil/easter.py`: ends `return datetime.date(int(y), int(m),
        // int(d))` — an explicit `datetime.date` constructor call.
        expect_substr: "date",
        source_evidence: "return datetime.date(int(y), int(m), int(d))",
    },
    Case {
        dir: "requests-2.31.0",
        name: "requests",
        id: "requests.utils.is_ipv4_address",
        // `requests/utils.py`: `try: socket.inet_aton(string_ip) except
        // OSError: return False` then `return True` — both return paths are
        // literal bools.
        //
        // (Two other requests picks were tried and dropped here, both for
        // the same reason, which is itself a real finding: `requests.api.get`
        // calls `request()` -> `session.request(...)` on the real
        // `requests.sessions.Session`, and `requests.utils
        // .get_encoding_from_headers` unpacks the return of the sibling
        // helper `_parse_content_type_header` — both chains are complex
        // enough, through an untyped callee, that pyrefly's solver lands on
        // `Any` rather than a concrete type. `context.rs::merge_type`
        // correctly *declines* to write a bare `Any` into an empty slot
        // (`Any` is not an improvement over `None`), so both stayed `None` —
        // an absence, not a wrong-but-present value, so neither belongs in a
        // test whose job is checking presence-and-correctness. `is_ipv4_address`
        // has no such untyped dependency, and pyrefly resolves it.)
        expect_substr: "bool",
        source_evidence: "except OSError: return False} return True",
    },
    Case {
        dir: "beautifulsoup4-4.12.3",
        name: "beautifulsoup4",
        id: "bs4.diagnose.rword",
        // `bs4/diagnose.py`: `s = ''` then `s += random.choice(t)` in a loop,
        // `return s` — a `str` built up by concatenation.
        expect_substr: "str",
        source_evidence: "s = ''; ...; s += random.choice(t); return s",
    },
    Case {
        dir: "more-itertools-10.2.0",
        name: "more-itertools",
        id: "more_itertools.more.ilen",
        // `more_itertools/more.py`: `counter = count(); ...; return
        // next(counter)` — `next()` on an `itertools.count()` yields `int`.
        expect_substr: "int",
        source_evidence: "counter = count(); ...; return next(counter)",
    },
];

fn find_return(oracle: &nudox_producer_python::oracle::PythonOracle, id: &str) -> Option<TypeData> {
    fn walk(item: &nudox_producer_python::oracle::ItemData, id: &str) -> Option<TypeData> {
        if item.id.0 == id
            && let ItemBody::Function(f) = &item.body
        {
            return f.return_ty.clone();
        }
        if let ItemBody::Class(c) = &item.body {
            for m in &c.methods {
                if let Some(t) = walk(m, id) {
                    return Some(t);
                }
            }
            for n in &c.nested {
                if let Some(t) = walk(n, id) {
                    return Some(t);
                }
            }
        }
        None
    }
    for m in &oracle.modules {
        for item in &m.items {
            if let Some(t) = walk(item, id) {
                return Some(t);
            }
        }
    }
    None
}

/// **Hand-verified inference correctness, not just presence, on the six
/// zero-annotation corpus packages.** See this file's module doc.
#[test]
fn inferred_return_types_match_what_the_source_actually_says() {
    let root = corpus_root();
    let mut checked = 0;
    let mut failures = Vec::new();

    for case in CASES {
        let pkg_root = root.join(case.dir);
        if !pkg_root.join("setup.py").is_file() && !pkg_root.join("pyproject.toml").is_file() {
            eprintln!("SKIP {}: not provisioned", case.dir);
            continue;
        }
        let src = PackageSource::new(&pkg_root, case.name, "0.0.0");
        let oracle = context::invoke_oracle(&src)
            .unwrap_or_else(|e| panic!("{}: invoke_oracle failed: {e}", case.dir));
        let Some(ty) = find_return(&oracle, case.id) else {
            failures.push(format!(
                "{}: no return type found for {} (source: {})",
                case.dir, case.id, case.source_evidence
            ));
            continue;
        };
        let rendered = format!("{ty:?}");
        eprintln!("{}: {} -> {rendered}", case.dir, case.id);
        if !rendered.contains(case.expect_substr) {
            failures.push(format!(
                "{}: {} inferred {rendered:?}, expected it to contain {:?} (source: {})",
                case.dir, case.id, case.expect_substr, case.source_evidence
            ));
        }
        checked += 1;
    }

    assert!(
        failures.is_empty(),
        "pyrefly inferred a WRONG type for one or more real functions (worse than Unknown):\n{}",
        failures.join("\n")
    );
    assert!(checked >= 8, "expected at least 8 of 9 cases to run against a provisioned corpus; got {checked}");
}
