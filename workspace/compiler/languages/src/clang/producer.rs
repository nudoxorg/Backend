//! [`Producer`] implementation for the C/C++ clang tier.

use crate::{PackageSource, Producer, ProducerError, ProducerId};
use clang::{Clang, Index};
use nudox_ir::{body::Language, lower::Lowering};

use crate::clang::{
    compile_commands::CompileCommands,
    extract::extract_file,
    lower::lower_oracle,
    oracle::{ClangOracle, Usr},
    system_includes,
};

// ── Producer
// ──────────────────────────────────────────────────────────────────

/// The C/C++ producer.
///
/// # libclang availability
///
/// This producer links dynamically against libclang.  The `clang` crate
/// locates libclang at **runtime** (via `dlopen` / `LoadLibrary`) using the
/// `LIBCLANG_PATH` environment variable or well-known OS search paths; it does
/// **not** require libclang at *build* time — the build just compiles Rust
/// code.  If libclang is absent at runtime, [`ClangProducer::invoke`] returns
/// a [`ProducerError::OracleSpawn`] error.
pub struct ClangProducer;

impl ClangProducer {
    pub fn new() -> Self {
        ClangProducer
    }
}

impl Default for ClangProducer {
    fn default() -> Self {
        Self::new()
    }
}

impl Producer for ClangProducer {
    type Id = Usr;
    type Oracle = ClangOracle;

    const ID: ProducerId = ProducerId("clang-libclang/1");
    const LANGUAGE: Language = Language::C; // also covers C++ files

    fn invoke(&self, src: &PackageSource) -> Result<ClangOracle, ProducerError> {
        // `Clang::new()` dynamically loads libclang.  If libclang is absent,
        // it returns an error string; map that to OracleSpawn.
        let clang = Clang::new().map_err(|e| ProducerError::OracleSpawn {
            command: "libclang".to_owned(),
            reason: std::io::Error::new(std::io::ErrorKind::NotFound, e),
        })?;

        let index = Index::new(&clang, false, false);
        let root = src.root();

        // Translation units: every `.c`/`.cpp` plus every package-root header,
        // each header once. A header included from a source is not that
        // source's main file, so `is_in_main_file` would drop it; opening the
        // header itself is what keeps a header-only API. System headers are
        // outside this walk.
        let sources: Vec<_> = find_sources(root);
        let cpp_package = sources.iter().any(|p| is_cpp(p) || is_cpp_header(p));

        // A real project's own build knows its `-I`/`-D`/`-std` flags; a
        // bare per-extension default cannot (see `compile_commands`'s module
        // docs for why that matters). Load it once per `invoke`, not once
        // per file.
        let compile_db = CompileCommands::load(root);

        // The host toolchain's own `-isystem` search path — see
        // `system_includes`'s module docs. Discovered (and cached) once;
        // every file gets it ahead of its own `-I`/`-isystem` flags so a
        // project's own headers still take priority via `-I`'s normal
        // higher search precedence over `-isystem`.
        let system_args = system_includes::args();

        let mut oracle = ClangOracle::default();

        for path in &sources {
            let db_args = compile_db.as_ref().and_then(|db| db.args_for(path));
            let default_args = default_args_for(path, cpp_package);
            let file_args = db_args.as_deref().unwrap_or(&default_args);

            let args: Vec<&str> = system_args
                .iter()
                .map(String::as_str)
                .chain(file_args.iter().map(String::as_str))
                .collect();

            let partial = extract_file(&index, path, &args);
            merge_oracle(&mut oracle, partial);
        }

        Ok(oracle)
    }

    fn lower(&self, oracle: &ClangOracle, out: &mut Lowering<Usr>) -> Result<(), ProducerError> {
        lower_oracle(oracle, out);
        Ok(())
    }
}

// ── Helpers
// ───────────────────────────────────────────────────────────────────

fn find_sources(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut sources = Vec::new();
    let mut headers = Vec::new();
    let mut seen = std::collections::HashSet::new();
    collect_sources(root, &mut sources, &mut headers, &mut seen);
    // Headers after sources. Each package-root header is one translation unit,
    // not one parse per file that includes it. A `.nudox_probe.cpp` twin is
    // not how a header becomes the main file — the header path is.
    sources.append(&mut headers);
    sources
}

fn collect_sources(
    dir: &std::path::Path,
    sources: &mut Vec<std::path::PathBuf>,
    headers: &mut Vec<std::path::PathBuf>,
    seen: &mut std::collections::HashSet<std::path::PathBuf>,
) {
    enum Kind {
        Source,
        Header,
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_sources(&path, sources, headers, seen);
            continue;
        }
        let kind = if is_cpp(&path) || is_c(&path) {
            Some(Kind::Source)
        } else if is_header(&path) {
            Some(Kind::Header)
        } else {
            None
        };
        let Some(kind) = kind else {
            continue;
        };
        // Canonical key so a header reached by two directory entries is still
        // one parse. The path we open is the one the walk found.
        let key = path.canonicalize().unwrap_or_else(|_| path.clone());
        if seen.insert(key) {
            match kind {
                Kind::Source => sources.push(path),
                Kind::Header => headers.push(path),
            }
        }
    }
}

/// The argument list used when a file has no `compile_commands.json` entry —
/// exactly what every `.c`/`.cpp` file used before ingestion existed.
///
/// Headers need an explicit language. `.hpp` is C++. A `.h` next to C++
/// sources is C++ too; a `.h` in a C-only tree stays C, matching `.c`.
fn default_args_for(path: &std::path::Path, cpp_package: bool) -> Vec<String> {
    if prefers_cpp(path, cpp_package) {
        vec!["-std=c++17".to_owned(), "-x".to_owned(), "c++".to_owned()]
    } else {
        vec!["-std=c11".to_owned()]
    }
}

fn prefers_cpp(path: &std::path::Path, cpp_package: bool) -> bool {
    is_cpp(path) || is_cpp_header(path) || (is_c_header(path) && cpp_package)
}

fn is_cpp(path: &std::path::Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("cpp" | "cxx" | "cc" | "C" | "c++")
    )
}

fn is_c(path: &std::path::Path) -> bool {
    matches!(path.extension().and_then(|e| e.to_str()), Some("c"))
}

fn is_cpp_header(path: &std::path::Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("hpp" | "hxx" | "hh" | "h++" | "H")
    )
}

fn is_c_header(path: &std::path::Path) -> bool {
    matches!(path.extension().and_then(|e| e.to_str()), Some("h"))
}

fn is_header(path: &std::path::Path) -> bool {
    is_c_header(path) || is_cpp_header(path)
}

/// Merge a partial oracle from one TU into the accumulator.
fn merge_oracle(acc: &mut ClangOracle, partial: ClangOracle) {
    acc.namespaces.extend(partial.namespaces);
    acc.records.extend(partial.records);
    acc.fields.extend(partial.fields);
    acc.functions.extend(partial.functions);
    acc.enums.extend(partial.enums);
    acc.variants.extend(partial.variants);
    acc.aliases.extend(partial.aliases);
    acc.vars.extend(partial.vars);
    acc.references.extend(partial.references);
    // One entry per parse, including a repeated header. Deduping here would
    // hide "once per includer".
    acc.main_files.extend(partial.main_files);

    // Dedup by USR — multiple TUs may see the same header declaration.
    dedup_by_usr(&mut acc.namespaces, |n| &n.usr);
    dedup_by_usr(&mut acc.records, |r| &r.usr);
    dedup_by_usr(&mut acc.fields, |f| &f.usr);
    dedup_by_usr(&mut acc.functions, |f| &f.usr);
    dedup_by_usr(&mut acc.enums, |e| &e.usr);
    dedup_by_usr(&mut acc.variants, |v| &v.usr);
    dedup_by_usr(&mut acc.aliases, |a| &a.usr);
    dedup_by_usr(&mut acc.vars, |v| &v.usr);
}

fn dedup_by_usr<T, F: Fn(&T) -> &str>(items: &mut Vec<T>, key: F) {
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    items.retain(|item| seen.insert(key(item).to_owned()));
}
