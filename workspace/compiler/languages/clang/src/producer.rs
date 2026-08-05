//! [`Producer`] implementation for the C/C++ clang tier.

use clang::{Clang, Index};
use nudox_ir::body::Language;
use nudox_ir::lower::Lowering;
use nudox_producer::{PackageSource, Producer, ProducerError, ProducerId};

use crate::{
    extract::extract_file,
    lower::lower_oracle,
    oracle::{ClangOracle, Usr},
};

// ── Producer ──────────────────────────────────────────────────────────────────

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

        // Find all C/C++ source files under the package root.
        let sources: Vec<_> = find_sources(root);

        let mut oracle = ClangOracle::default();

        for path in &sources {
            // Choose C or C++ parse mode based on file extension.
            let args: &[&str] = if is_cpp(path) {
                &["-std=c++17", "-x", "c++"]
            } else {
                &["-std=c11"]
            };

            let partial = extract_file(&index, path, args);
            merge_oracle(&mut oracle, partial);
        }

        Ok(oracle)
    }

    fn lower(&self, oracle: &ClangOracle, out: &mut Lowering<Usr>) -> Result<(), ProducerError> {
        lower_oracle(oracle, out);
        Ok(())
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn find_sources(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut result = Vec::new();
    collect_sources(root, &mut result);
    result
}

fn collect_sources(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_sources(&path, out);
        } else if is_cpp(&path) || is_c(&path) {
            out.push(path);
        }
    }
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
