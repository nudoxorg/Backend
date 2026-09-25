//! Sweeps every provisioned `cpp` corpus package through the full
//! `nudox_languages::produce` pipeline (`invoke` -> `lower` -> `finish`)
//! against real, third-party checkouts under `result/`.
//!
//! # Why this file does more than `real_package.rs`
//!
//! `real_package.rs` proves the pipeline end to end on a hand-authored,
//! structurally-real fixture. This file proves it (or does not) on the 20
//! real corpus packages listed for `ecosystem = "cpp"` in
//! `nix/corpus.nix` — per docs/AGENTS-DOCTRINE.md §4, a producer's own
//! green fixture suite tells you nothing about real code.
//!
//! Two producer-level bugs were found and fixed while building this file (see
//! `src/extract.rs`'s `is_in_main_file` and `src/system_includes.rs`), and a
//! third gap — real C/C++ projects need `-I` flags the on-disk layout alone
//! does not reveal — is what the provisioning below closes per-package. None
//! of the 20 packages below resolved at all before those three fixes.
//!
//! # Provisioning: why "shim" files
//!
//! `find_sources` treats each package-root header as its own translation unit
//! (once, not once per includer), and `extract.rs`'s visitor only ever emits
//! declarations physically written in *that* main file (see
//! `is_in_main_file`'s doc comment) — never declarations merely reachable
//! through a `#include`. A header-only library's public API is visited because
//! the header itself is opened, not because a `.nudox_probe.cpp` twin was
//! written next to it. The shim below still exists so a
//! `compile_commands.json` entry can attach that package's `-I` flags to a
//! copy of the header; it is not what makes the header a main file.
//!
//! [`provision_shims`] makes that true, idempotently, for every header under
//! each package's `header_roots`: it writes a same-directory twin file
//! (`Foo.hpp` -> `Foo.nudox_probe.cpp`, byte-identical to the original) so
//! `find_sources`'s ordinary directory walk picks it up as its own
//! translation unit. This is the same shape of fixup as the Rust corpus's
//! `[workspace]`-table append (docs/AGENTS-DOCTRINE.md §8) — a small, persistent,
//! idempotent addition to a `result/` checkout that makes an otherwise-
//! unusable real package usable, not a change to the package's own tracked
//! content.
//!
//! [`write_compile_commands`] then emits one `compile_commands.json` entry
//! per `.c`/`.cpp` file under the package root (originals and shims alike)
//! carrying that package's `-I` flags, so `CompileCommands::load` (see
//! `src/compile_commands.rs`) can supply them — without this, every angle-
//! bracket cross-header `#include` (e.g. `#include <CLI/App.hpp>`) fails
//! with a fatal "file not found" and that one translation unit lowers empty.
//!
//! # Scope note: three packages are shimmed over a representative subset
//!
//! `abseil-cpp` (385 headers), `range-v3` (328), and `glm` (~200 non-`.inl`
//! headers) are large enough that shimming every header would make this
//! suite's runtime dominated by parsing, not by anything this file is
//! actually trying to prove. Each is shimmed over one real, representative
//! subdirectory instead (see their `Entry::header_roots`) — enough to prove
//! the mechanism resolves real symbols end-to-end for that package, not an
//! exhaustive extraction of its full surface. This is a real, disclosed
//! scope limitation, not a silent one.
use std::path::{Path, PathBuf};

use nudox_ir::change::{EcosystemId, PackageLineageId, PackageName};
use nudox_languages::clang::ClangProducer;
use nudox_languages::{PackageSource, produce};

/// `clang::Clang` allows only one instance per process — see
/// `src/tests/mod.rs`'s `require_clang` doc comment. `cargo test`'s default
/// harness runs every `#[test]` in this binary concurrently on separate
/// threads, so every entry must serialize on this guard.
static CLANG_SINGLETON: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Clone, Copy)]
enum Lang {
    Cpp,
    C,
}

/// One corpus entry: where it lives under `result/`, which
/// directories/files hold the public headers worth shimming (see module
/// docs), which `-I` roots its own cross-includes need, and one real,
/// specific symbol that must survive lowering — the doctrine §4 content
/// assertion ("a symbol that really exists"), not a bare non-zero count.
struct Entry {
    dir: &'static str,
    name: &'static str,
    version: &'static str,
    header_roots: &'static [&'static str],
    include_dirs: &'static [&'static str],
    lang: Lang,
    expect_symbol: &'static str,
    min_entries: usize,
}

const ENTRIES: &[Entry] = &[
    Entry {
        dir: "nlohmann-json-v3.11.3",
        name: "nlohmann-json",
        version: "v3.11.3",
        header_roots: &["single_include/nlohmann"],
        include_dirs: &["single_include"],
        lang: Lang::Cpp,
        expect_symbol: "basic_json",
        min_entries: 500,
    },
    Entry {
        dir: "simdjson-v4.6.6",
        name: "simdjson",
        version: "v4.6.6",
        header_roots: &["simdjson.h"],
        include_dirs: &["."],
        lang: Lang::Cpp,
        expect_symbol: "padded_string",
        min_entries: 200,
    },
    Entry {
        dir: "fmt-12.2.0",
        name: "fmt",
        version: "12.2.0",
        header_roots: &["include/fmt"],
        include_dirs: &["include"],
        lang: Lang::Cpp,
        expect_symbol: "format_error",
        min_entries: 200,
    },
    Entry {
        dir: "zlib-v1.3.2",
        name: "zlib",
        version: "v1.3.2",
        header_roots: &["zlib.h", "zconf.h"],
        include_dirs: &["."],
        lang: Lang::C,
        expect_symbol: "deflate",
        min_entries: 20,
    },
    Entry {
        dir: "zstd-v1.5.7",
        name: "zstd",
        version: "v1.5.7",
        header_roots: &["lib/zstd.h"],
        include_dirs: &["lib"],
        lang: Lang::C,
        expect_symbol: "ZSTD_compress",
        min_entries: 20,
    },
    Entry {
        dir: "lz4-v1.10.0",
        name: "lz4",
        version: "v1.10.0",
        header_roots: &["lib/lz4.h"],
        include_dirs: &["lib"],
        lang: Lang::C,
        expect_symbol: "LZ4_compress_default",
        min_entries: 10,
    },
    Entry {
        dir: "cli11-v2.7.2",
        name: "cli11",
        version: "v2.7.2",
        header_roots: &["include/CLI"],
        include_dirs: &["include"],
        lang: Lang::Cpp,
        expect_symbol: "App",
        min_entries: 100,
    },
    Entry {
        dir: "googletest-v1.17.0",
        name: "googletest",
        version: "v1.17.0",
        header_roots: &["googletest/include/gtest", "googletest/src"],
        include_dirs: &["googletest/include", "googletest"],
        lang: Lang::Cpp,
        expect_symbol: "Test",
        min_entries: 200,
    },
    Entry {
        dir: "magic-enum-v0.9.8",
        name: "magic-enum",
        version: "v0.9.8",
        header_roots: &["include/magic_enum"],
        include_dirs: &["include"],
        lang: Lang::Cpp,
        expect_symbol: "enum_name",
        min_entries: 20,
    },
    Entry {
        dir: "abseil-cpp-20260526.0",
        name: "abseil-cpp",
        version: "20260526.0",
        // See module docs: representative subset, not the whole 385-header
        // tree (`absl/strings` alone is 169 files; `absl/status` is 22 and
        // still exercises a real, cross-header-dependent class).
        header_roots: &["absl/status"],
        include_dirs: &["."],
        lang: Lang::Cpp,
        expect_symbol: "Status",
        min_entries: 30,
    },
    Entry {
        dir: "glm-1.0.3",
        name: "glm",
        version: "1.0.3",
        // See module docs: representative subset, not the whole ~200-header tree.
        header_roots: &[
            "glm/vec2.hpp",
            "glm/vec3.hpp",
            "glm/vec4.hpp",
            "glm/mat4x4.hpp",
            "glm/glm.hpp",
        ],
        include_dirs: &["."],
        lang: Lang::Cpp,
        expect_symbol: "vec",
        min_entries: 5,
    },
    Entry {
        dir: "catch2-v3.15.3",
        name: "catch2",
        version: "v3.15.3",
        header_roots: &["catch_amalgamated.hpp"],
        include_dirs: &["."],
        lang: Lang::Cpp,
        expect_symbol: "Session",
        min_entries: 200,
    },
    Entry {
        dir: "spdlog-v1.17.0",
        name: "spdlog",
        version: "v1.17.0",
        header_roots: &["include/spdlog"],
        include_dirs: &["include"],
        lang: Lang::Cpp,
        expect_symbol: "logger",
        min_entries: 100,
    },
    Entry {
        dir: "cxxopts-v3.3.1",
        name: "cxxopts",
        version: "v3.3.1",
        header_roots: &["include/cxxopts.hpp"],
        include_dirs: &["include"],
        lang: Lang::Cpp,
        expect_symbol: "Options",
        min_entries: 10,
    },
    Entry {
        dir: "tomlplusplus-v3.4.0",
        name: "tomlplusplus",
        version: "v3.4.0",
        header_roots: &["toml.hpp"],
        include_dirs: &["."],
        lang: Lang::Cpp,
        expect_symbol: "table",
        min_entries: 100,
    },
    Entry {
        dir: "date-v3.0.5",
        name: "date",
        version: "v3.0.5",
        header_roots: &["include/date"],
        include_dirs: &["include"],
        lang: Lang::Cpp,
        expect_symbol: "year_month_day",
        min_entries: 20,
    },
    Entry {
        dir: "cereal-v1.3.2",
        name: "cereal",
        version: "v1.3.2",
        header_roots: &[
            "include/cereal/archives",
            "include/cereal/types",
            "include/cereal/details",
        ],
        include_dirs: &["include"],
        lang: Lang::Cpp,
        expect_symbol: "BinaryOutputArchive",
        min_entries: 20,
    },
    Entry {
        dir: "range-v3-0.12.0",
        name: "range-v3",
        version: "0.12.0",
        // See module docs: representative subset, not the whole 328-header
        // tree (`view/` alone is 74 files, `algorithm/` 90; `functional/`
        // is 14 and still a real, cross-header-dependent slice).
        header_roots: &["include/range/v3/functional"],
        include_dirs: &["include"],
        lang: Lang::Cpp,
        expect_symbol: "compose_fn",
        min_entries: 10,
    },
    Entry {
        dir: "cpp-httplib-v0.52.0",
        name: "cpp-httplib",
        version: "v0.52.0",
        header_roots: &["httplib.h"],
        include_dirs: &["."],
        lang: Lang::Cpp,
        expect_symbol: "Server",
        min_entries: 50,
    },
    Entry {
        dir: "argparse-v3.2",
        name: "argparse",
        version: "v3.2",
        header_roots: &["include/argparse/argparse.hpp"],
        include_dirs: &["include"],
        lang: Lang::Cpp,
        expect_symbol: "ArgumentParser",
        min_entries: 10,
    },
];

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../result")
        .canonicalize()
        .expect("no result/ checkout — see docs/CORPUS.md to (re)provision it")
}

// ── Shim provisioning ────────────────────────────────────────────────────────

fn header_extensions(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::Cpp => &["h", "hpp", "hxx", "hh", "H"],
        Lang::C => &["h"],
    }
}

fn shim_extension(lang: Lang) -> &'static str {
    match lang {
        Lang::Cpp => "cpp",
        Lang::C => "c",
    }
}

/// Idempotently create a same-directory translation-unit twin for every
/// header under `header_roots` (files or directories, either is accepted) —
/// see module docs for why a `#include` alone cannot substitute for this.
/// Returns the twin files created/refreshed, so callers can scope
/// `compile_commands.json` to exactly the files this call means to cover
/// (see `write_compile_commands`'s size-threshold doc comment).
fn provision_shims(root: &Path, header_roots: &[&str], lang: Lang) -> Vec<PathBuf> {
    let mut shims = Vec::new();
    for rel in header_roots {
        let base = root.join(rel);
        if base.is_file() {
            if let Some(twin) = shim_one(&base, lang) {
                shims.push(twin);
            }
        } else if base.is_dir() {
            walk_and_shim(&base, lang, &mut shims);
        }
    }
    shims
}

fn walk_and_shim(dir: &Path, lang: Lang, shims: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_and_shim(&path, lang, shims);
        } else if is_header(&path, lang)
            && let Some(twin) = shim_one(&path, lang)
        {
            shims.push(twin);
        }
    }
}

fn is_header(path: &Path, lang: Lang) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    header_extensions(lang).contains(&ext)
}

/// Write (or refresh) `<stem>.nudox_probe.<ext>` next to `header`,
/// byte-identical to it, and return its path. Re-running this suite must
/// not churn mtimes needlessly, so an already-matching twin is left alone.
fn shim_one(header: &Path, lang: Lang) -> Option<PathBuf> {
    let stem = header.file_stem().and_then(|s| s.to_str())?;
    let twin = header.with_file_name(format!("{stem}.nudox_probe.{}", shim_extension(lang)));
    let bytes = std::fs::read(header).ok()?;
    if std::fs::read(&twin).ok().as_deref() != Some(bytes.as_slice()) {
        std::fs::write(&twin, &bytes).ok()?;
    }
    Some(twin)
}

// ── compile_commands.json generation ────────────────────────────────────────

fn is_cpp_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("cpp" | "cxx" | "cc" | "C" | "c++")
    )
}

fn is_c_file(path: &Path) -> bool {
    matches!(path.extension().and_then(|e| e.to_str()), Some("c"))
}

/// Mirrors `producer::find_sources`'s own walk exactly, so every file that
/// walk will visit gets a matching `compile_commands.json` entry — an
/// unlisted file silently falls back to bare `-std=`/`-x` defaults with no
/// `-I` at all (see `compile_commands.rs`'s own doc comment on
/// `args_for`), which is enough to make any angle-bracket cross-header
/// `#include` fail.
fn collect_translation_units(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_translation_units(&path, out);
        } else if is_cpp_file(&path) || is_c_file(&path) {
            out.push(path);
        }
    }
}

/// Above this many `.c`/`.cpp` files package-wide, giving every one of them
/// real `-I` flags would make this suite's runtime dominated by parsing
/// code well outside what any `Entry` actually claims to cover (this is
/// exactly what made an early version of this sweep spend several minutes
/// mid-way through `abseil-cpp`, parsing ~450 `.cc` files under
/// subdirectories no `Entry::header_roots` names). Past the threshold,
/// `write_compile_commands` scopes real `-I` treatment down to the files
/// `provision_shims` actually created plus whatever already-real sources
/// live under a directory-shaped `header_root` — everything else still gets
/// *visited* by `find_sources` (unavoidable: it walks the whole tree
/// unconditionally) but falls back to bare `-std=`/`-x` with no `-I`, so its
/// own cross-directory `#include`s fail fast instead of triggering a full,
/// slow, successful parse of code this sweep does not claim to cover.
const WHOLE_TREE_FILE_THRESHOLD: usize = 80;

/// Write a `compile_commands.json` at `root` giving the in-scope files
/// (see `WHOLE_TREE_FILE_THRESHOLD`) `-I<include_dirs>` plus a
/// language-appropriate `-std=`. Overwritten on every call (cheap, and the
/// file list can change as shims are added/removed between runs).
fn write_compile_commands(
    root: &Path,
    include_dirs: &[&str],
    header_roots: &[&str],
    shims: &[PathBuf],
) {
    let mut all_files = Vec::new();
    collect_translation_units(root, &mut all_files);

    let files: Vec<PathBuf> = if all_files.len() <= WHOLE_TREE_FILE_THRESHOLD {
        all_files
    } else {
        let dir_roots: Vec<PathBuf> = header_roots
            .iter()
            .map(|r| root.join(r))
            .filter(|p| p.is_dir())
            .collect();
        all_files
            .into_iter()
            .filter(|f| shims.contains(f) || dir_roots.iter().any(|d| f.starts_with(d)))
            .collect()
    };

    let entries: Vec<serde_json::Value> = files
        .iter()
        .map(|path| {
            let cpp = is_cpp_file(path);
            let mut arguments = vec![if cpp { "c++" } else { "cc" }.to_owned()];
            for inc in include_dirs {
                arguments.push(format!("-I{inc}"));
            }
            arguments.push(if cpp { "-std=c++17" } else { "-std=c11" }.to_owned());
            if cpp {
                arguments.push("-x".to_owned());
                arguments.push("c++".to_owned());
            }
            arguments.push("-c".to_owned());
            let rel = path.strip_prefix(root).unwrap_or(path);
            serde_json::json!({
                "directory": root.to_string_lossy(),
                "file": rel.to_string_lossy(),
                "arguments": arguments,
            })
        })
        .collect();

    std::fs::write(
        root.join("compile_commands.json"),
        serde_json::to_string_pretty(&entries).expect("serialize compile_commands.json"),
    )
    .expect("write compile_commands.json");
}

// ── Driving the pipeline ─────────────────────────────────────────────────────

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
        has_symbol: bool,
        shims_written: usize,
    },
    Fail {
        stage: &'static str,
        chain: String,
    },
}

fn run_entry(entry: &Entry) -> Outcome {
    let root = corpus_root().join(entry.dir);
    if !root.is_dir() {
        return Outcome::Fail {
            stage: "preflight",
            chain: format!(
                "no checkout at {} — run `nix build .#checks.corpus` from the repo root",
                root.display()
            ),
        };
    }

    let shims = provision_shims(&root, entry.header_roots, entry.lang);
    let shims_written = shims.len();
    write_compile_commands(&root, entry.include_dirs, entry.header_roots, &shims);

    let src = PackageSource::new(&root, entry.name, entry.version);
    let lid = PackageLineageId::new(EcosystemId::new("cpp"), PackageName::new(entry.name));

    let case = format!("cpp-sweep-{}", entry.dir);
    let (produced, _cost) = heart::cost::measured(&case, &root, || {
        produce(
            &ClangProducer::new(),
            &src,
            &lid,
            &nudox_ir::foreign::Unlinked,
        )
    });

    match produced {
        Ok(intro) => {
            let table_len = intro.table.len();
            let has_symbol = intro
                .table
                .iter()
                .any(|(_, e)| e.sym().name == entry.expect_symbol);
            Outcome::Ok {
                table_len,
                has_symbol,
                shims_written,
            }
        }
        Err(e) => Outcome::Fail {
            stage: "produce (invoke/lower/finish)",
            chain: chain(&e),
        },
    }
}

#[test]
fn every_provisioned_cpp_corpus_package_lowers_through_the_real_producer() {
    let _guard = CLANG_SINGLETON
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let mut failures: Vec<(&str, &str, String)> = Vec::new();
    let mut successes: Vec<(&str, usize)> = Vec::new();

    for entry in ENTRIES {
        eprintln!("=== {} ({} @ {}) ===", entry.dir, entry.name, entry.version);
        match run_entry(entry) {
            Outcome::Ok {
                table_len,
                has_symbol,
                shims_written,
            } => {
                eprintln!(
                    "OK  {}: table_len={table_len} shims_written={shims_written} \
                     has_expected_symbol({})={has_symbol}",
                    entry.dir, entry.expect_symbol
                );
                if table_len < entry.min_entries {
                    failures.push((
                        entry.dir,
                        "entry count floor",
                        format!(
                            "table has {table_len} entries, expected at least {} — see stderr \
                             for the run; this is not real extraction",
                            entry.min_entries
                        ),
                    ));
                    continue;
                }
                if !has_symbol {
                    failures.push((
                        entry.dir,
                        "content assertion",
                        format!(
                            "expected symbol {:?} not found among {table_len} lowered entries",
                            entry.expect_symbol
                        ),
                    ));
                    continue;
                }
                successes.push((entry.dir, table_len));
            }
            Outcome::Fail { stage, chain } => {
                eprintln!("FAIL {} at [{stage}]: {chain}", entry.dir);
                failures.push((entry.dir, stage, chain));
            }
        }
    }

    eprintln!(
        "\n=== cpp corpus sweep summary: {}/{} succeeded ===",
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
        "{} of {} cpp corpus entries failed; see stderr above for the full detail of each",
        failures.len(),
        ENTRIES.len()
    );
}
