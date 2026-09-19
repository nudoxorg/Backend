//! Ingest a `compile_commands.json` (the [JSON Compilation Database][spec]
//! format CMake, Bear, and Ninja all know how to emit) so real projects get
//! their real per-file include paths instead of the two hardcoded
//! `-std=c11`/`-std=c++17` argument lists [`super::producer`] falls back to.
//!
//! [spec]: https://clang.llvm.org/docs/JSONCompilationDatabase.html
//!
//! # Why this exists
//!
//! [`super::producer::find_sources`] discovers translation units by walking
//! the package root for `.c`/`.cpp` extensions — it has no way to know a
//! project's `-I`/`-isystem` include paths, because those live outside the
//! source tree, in whatever built the project. A real project almost always
//! splits headers from implementation (`include/` vs `src/`, or a `vendor/`
//! tree, or an unrelated system dependency's headers) and passes `-I` flags
//! for all of it. Without them, libclang cannot resolve `#include "..."` /
//! `#include <...>` directives that reach outside the single file's own
//! directory: it treats the miss as a diagnostic, not a hard parse error
//! ([`super::extract::extract_file`] does not currently surface parse
//! diagnostics — see that function), so the practical effect is silent:
//! declarations that depend on the missing header's types are skipped or
//! degrade to [`crate::clang::oracle::OracleType::Inferred`], not an error a caller
//! can see.
//!
//! # What this does *not* do
//!
//! This ingests `compile_commands.json` only, not `CMakeLists.txt` directly.
//! `compile_commands.json` is the tool-agnostic, already-standard artifact
//! that CMake (`-DCMAKE_EXPORT_COMPILE_COMMANDS=ON`), Bear, Meson, and Bazel
//! (via `bazel-compile-commands-extractor`) all already know how to produce;
//! re-implementing enough of CMake's own configure step to derive include
//! paths from a raw `CMakeLists.txt` without running CMake is a whole
//! separate, much larger project, not a gap this crate's scope covers.
//! Projects that build with CMake but have not generated the database yet
//! still fall back to the hardcoded defaults, same as before this module
//! existed.
//!
//! This also does not change *which* files [`super::producer::find_sources`]
//! decides are translation units — a `compile_commands.json` entry only
//! enriches the argument list for a file the directory walk already found;
//! it is not (yet) used as the authoritative file list. A project whose real
//! build compiles a strict subset of the `.c`/`.cpp` files under its root
//! (platform-specific files behind build-system `if`s, vendored files not
//! meant to be part of the build, etc.) will still have every such file
//! walked and parsed with best-effort default arguments, exactly as before.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use serde::Deserialize;

/// One entry in the `compile_commands.json` array, before path resolution.
///
/// Either `arguments` (the modern, unambiguous array form) or `command` (the
/// legacy single shell-escaped string) is present — never neither, per the
/// spec. `#[serde(default)]` on both lets a file that only has one of them
/// deserialize without the other field's absence being an error.
#[derive(Debug, Deserialize)]
struct RawEntry {
    directory: String,
    file: String,
    #[serde(default)]
    arguments: Option<Vec<String>>,
    #[serde(default)]
    command: Option<String>,
}

/// A parsed `compile_commands.json`, indexed by canonicalized source path.
#[derive(Debug, Default)]
pub(crate) struct CompileCommands {
    by_file: HashMap<PathBuf, Vec<String>>,
}

impl CompileCommands {
    /// Look for `compile_commands.json` at the package root, or in the
    /// `nix/build/` subdirectory `cmake -B build
    /// -DCMAKE_EXPORT_COMPILE_COMMANDS=ON` writes it to — the two locations
    /// real C/C++ tooling actually produces by default. Returns `None` if
    /// neither location has a parseable database; callers fall back to
    /// [`super::producer`]'s hardcoded `-std=` defaults per file, exactly as
    /// if this module did not run.
    pub(crate) fn load(root: &Path) -> Option<Self> {
        let candidates = [
            root.join("compile_commands.json"),
            root.join("build").join("compile_commands.json"),
        ];
        let db_path = candidates.into_iter().find(|p| p.is_file())?;
        let text = std::fs::read_to_string(&db_path).ok()?;
        let entries: Vec<RawEntry> = serde_json::from_str(&text).ok()?;

        let mut by_file = HashMap::new();
        for entry in entries {
            let source_path = resolve_entry_path(&entry);
            let Ok(canon) = source_path.canonicalize() else {
                // A stale entry (file since deleted/moved) — skip rather
                // than fail the whole database.
                continue;
            };
            let raw_args = entry.arguments.unwrap_or_else(|| {
                entry
                    .command
                    .as_deref()
                    .map(split_command_line)
                    .unwrap_or_default()
            });
            let stripped = filter_relevant_args(&raw_args, &entry.file);
            let resolved = rewrite_relative_include_paths(&stripped, Path::new(&entry.directory));
            by_file.insert(canon, resolved);
        }
        Some(Self { by_file })
    }

    /// The real build's argument list for `path`, if it appears in the
    /// database. `None` means "not listed here" — callers fall back to the
    /// per-extension defaults, not an empty argument list, so an unlisted
    /// file is still parsed rather than silently dropped.
    pub(crate) fn args_for(&self, path: &Path) -> Option<Vec<String>> {
        let canon = path.canonicalize().ok()?;
        self.by_file.get(&canon).cloned()
    }
}

/// Resolve a raw entry's `file` against its `directory`, per the spec: `file`
/// may be relative (to `directory`) or already absolute.
fn resolve_entry_path(entry: &RawEntry) -> PathBuf {
    let file = PathBuf::from(&entry.file);
    if file.is_absolute() {
        file
    } else {
        PathBuf::from(&entry.directory).join(file)
    }
}

/// Minimal whitespace tokenizer for the legacy `command` string field.
///
/// Does not handle quoting or escaping — a flag value containing a space
/// (e.g. `-DFOO="a b"`) will split incorrectly. This is a known, accepted
/// limitation: `arguments` is the modern, unambiguous field precisely to
/// avoid needing a real shell lexer here, and every fixture and real
/// generator this was tested against (CMake, Bear) emits `arguments`, not
/// `command`. Prefer generating `compile_commands.json` with `arguments` if
/// you control the generator.
fn split_command_line(cmd: &str) -> Vec<String> {
    cmd.split_whitespace().map(str::to_owned).collect()
}

/// Strip the parts of a real compiler invocation that `extract_file` already
/// supplies on its own (the compiler executable, the source file, `-c`, and
/// `-o <output>`), keeping everything else — `-I`, `-isystem`, `-D`, `-std`,
/// and any flag libclang does not recognize (it tolerates unknown flags).
fn filter_relevant_args(raw: &[String], entry_file: &str) -> Vec<String> {
    let file_name = Path::new(entry_file)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(entry_file);

    let mut out = Vec::with_capacity(raw.len());
    let mut skip_next = false;
    for (i, arg) in raw.iter().enumerate() {
        if skip_next {
            skip_next = false;
            continue;
        }
        if i == 0 {
            // argv[0]: the compiler executable itself.
            continue;
        }
        if arg == "-c" {
            continue;
        }
        if arg == "-o" {
            skip_next = true;
            continue;
        }
        // The source file itself, however it was spelled (absolute,
        // relative, just the basename).
        if arg == entry_file
            || arg.ends_with(file_name)
                && Path::new(arg).file_name().and_then(|f| f.to_str()) == Some(file_name)
        {
            continue;
        }
        out.push(arg.clone());
    }
    out
}

/// Include-path flags whose value is a path, per Clang's own driver options.
/// Each has both a joined (`-Ipath`) and, except `-I`'s alternate spelling,
/// a separate-argument (`-I path`) form; both are handled below.
const INCLUDE_PATH_FLAGS: &[&str] = &["-I", "-isystem", "-iquote", "-idirafter"];

/// Rewrite every relative `-I`/`-isystem`/`-iquote`/`-idirafter` value to an
/// absolute path anchored at `directory`.
///
/// The JSON Compilation Database spec defines `directory` as "the working
/// directory... commands are executed from" — every relative path in
/// `arguments`/`command` is only meaningful relative to it. `clang::Parser`
/// has no way to set libclang's notion of a working directory; it just hands
/// the argument strings to `clang_parseTranslationUnit`, which resolves any
/// relative `-I` against *this process's* current directory instead — almost
/// never the same place, since the parsing process is a long-lived test
/// binary or daemon, not a fresh compiler invocation `cd`'d into the
/// project. Left unresolved, this makes every relative `-I` in a real
/// `compile_commands.json` (the overwhelming majority — CMake, Bear, and
/// friends all emit them relative to `directory`) silently fail to find its
/// headers, no better than not reading the database at all. Rewriting them
/// here, once, at ingestion time, is what makes the flags actually usable.
fn rewrite_relative_include_paths(args: &[String], directory: &Path) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];

        // Joined form: `-Ipath`, `-isystempath`, ...
        if let Some(flag) = INCLUDE_PATH_FLAGS
            .iter()
            .find(|f| arg.starts_with(**f) && arg.len() > f.len())
        {
            let value = &arg[flag.len()..];
            out.push(format!("{flag}{}", resolve_relative(value, directory)));
            i += 1;
            continue;
        }

        // Separate-argument form: `-I path`, `-isystem path`, ...
        if INCLUDE_PATH_FLAGS.contains(&arg.as_str()) && i + 1 < args.len() {
            out.push(arg.clone());
            out.push(resolve_relative(&args[i + 1], directory));
            i += 2;
            continue;
        }

        out.push(arg.clone());
        i += 1;
    }
    out
}

fn resolve_relative(value: &str, directory: &Path) -> String {
    let p = Path::new(value);
    if p.is_absolute() {
        value.to_owned()
    } else {
        directory.join(p).to_string_lossy().into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_database_yields_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(CompileCommands::load(dir.path()).is_none());
    }

    #[test]
    fn arguments_form_is_indexed_by_canonical_path_with_source_and_driver_stripped() {
        let dir = tempfile::tempdir().unwrap();
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let file_path = src_dir.join("main.c");
        std::fs::write(&file_path, "int main(void) { return 0; }\n").unwrap();

        let db = serde_json::json!([
            {
                "directory": dir.path().to_str().unwrap(),
                "file": "src/main.c",
                "arguments": ["cc", "-Iinclude", "-DFOO=1", "-std=c11", "-c", "-o", "main.o", "src/main.c"],
            }
        ]);
        std::fs::write(dir.path().join("compile_commands.json"), db.to_string()).unwrap();

        let cc = CompileCommands::load(dir.path()).expect("database must load");
        let args = cc.args_for(&file_path).expect("main.c must be listed");

        // `-Iinclude` is relative to `directory` per the spec, so it must
        // come out rewritten to an absolute path, not the literal string —
        // see `rewrite_relative_include_paths`.
        let expected_include = format!("-I{}", dir.path().join("include").display());
        assert!(args.contains(&expected_include), "got {args:?}");
        assert!(args.contains(&"-DFOO=1".to_owned()), "got {args:?}");
        assert!(args.contains(&"-std=c11".to_owned()), "got {args:?}");
        assert!(
            !args.contains(&"cc".to_owned()),
            "driver must be stripped: {args:?}"
        );
        assert!(
            !args.contains(&"-c".to_owned()),
            "-c must be stripped: {args:?}"
        );
        assert!(
            !args.contains(&"-o".to_owned()),
            "-o must be stripped: {args:?}"
        );
        assert!(
            !args.contains(&"main.o".to_owned()),
            "-o's arg must be stripped: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a.ends_with("main.c")),
            "source file must be stripped (extract_file supplies it separately): {args:?}"
        );
    }

    #[test]
    fn command_form_is_split_and_filtered_the_same_way() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("lib.cpp");
        std::fs::write(&file_path, "int f() { return 0; }\n").unwrap();

        let db = serde_json::json!([
            {
                "directory": dir.path().to_str().unwrap(),
                "file": "lib.cpp",
                "command": "c++ -Iinclude -std=c++17 -c -o lib.o lib.cpp",
            }
        ]);
        std::fs::write(dir.path().join("compile_commands.json"), db.to_string()).unwrap();

        let cc = CompileCommands::load(dir.path()).expect("database must load");
        let args = cc.args_for(&file_path).expect("lib.cpp must be listed");
        let expected_include = format!("-I{}", dir.path().join("include").display());
        assert!(args.contains(&expected_include), "got {args:?}");
        assert!(args.contains(&"-std=c++17".to_owned()), "got {args:?}");
    }

    #[test]
    fn relative_include_paths_are_rewritten_absolute_against_directory() {
        // The bug this guards: `clang::Parser` has no notion of the
        // database's `directory` — a relative `-I` handed to libclang
        // unmodified resolves against *this process's* cwd, not the
        // project's, so headers fail to resolve even though the flag is
        // technically present. This was caught empirically (a hand-built
        // fixture with a real `-Iinclude` entry produced zero declarations
        // until this rewrite existed) — see module docs.
        let directory = Path::new("/project/root");
        let args = vec![
            "cc".to_owned(),
            "-Iinclude".to_owned(),
            "-I".to_owned(),
            "vendor/include".to_owned(),
            "-isystem".to_owned(),
            "third_party".to_owned(),
            "-I/already/absolute".to_owned(),
            "-std=c11".to_owned(),
        ];
        let rewritten = rewrite_relative_include_paths(&args, directory);
        assert_eq!(
            rewritten,
            vec![
                "cc".to_owned(),
                "-I/project/root/include".to_owned(),
                "-I".to_owned(),
                "/project/root/vendor/include".to_owned(),
                "-isystem".to_owned(),
                "/project/root/third_party".to_owned(),
                "-I/already/absolute".to_owned(),
                "-std=c11".to_owned(),
            ]
        );
    }

    #[test]
    fn unlisted_file_yields_none_not_empty_args() {
        let dir = tempfile::tempdir().unwrap();
        let listed = dir.path().join("listed.c");
        let unlisted = dir.path().join("unlisted.c");
        std::fs::write(&listed, "").unwrap();
        std::fs::write(&unlisted, "").unwrap();

        let db = serde_json::json!([
            {
                "directory": dir.path().to_str().unwrap(),
                "file": "listed.c",
                "arguments": ["cc", "-std=c11", "-c", "-o", "listed.o", "listed.c"],
            }
        ]);
        std::fs::write(dir.path().join("compile_commands.json"), db.to_string()).unwrap();

        let cc = CompileCommands::load(dir.path()).expect("database must load");
        assert!(cc.args_for(&listed).is_some());
        assert!(
            cc.args_for(&unlisted).is_none(),
            "a file absent from the database must fall back to defaults, not an empty arg list"
        );
    }

    #[test]
    fn database_in_build_subdirectory_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let build_dir = dir.path().join("build");
        std::fs::create_dir_all(&build_dir).unwrap();
        let file_path = dir.path().join("a.c");
        std::fs::write(&file_path, "").unwrap();

        let db = serde_json::json!([
            {
                "directory": dir.path().to_str().unwrap(),
                "file": "a.c",
                "arguments": ["cc", "-std=c11", "-c", "a.c"],
            }
        ]);
        std::fs::write(build_dir.join("compile_commands.json"), db.to_string()).unwrap();

        let cc =
            CompileCommands::load(dir.path()).expect("database under nix/build/ must be found");
        assert!(cc.args_for(&file_path).is_some());
    }
}
