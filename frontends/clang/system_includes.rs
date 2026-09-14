//! Discover the host toolchain's default system include search path.
//!
//! # The gap this closes
//!
//! [`super::producer::ClangProducer::invoke`] calls `clang_parseTranslationUnit`
//! directly through libclang's C API — there is no shell in the loop, and
//! critically, no driver *wrapper script* in the loop either. On a
//! system whose `clang`/`cc` on `PATH` is such a wrapper — nix's
//! `cc-wrapper` is exactly this shape — the wrapper is what injects the
//! toolchain's own `-isystem`/`-isysroot` flags (derived from its build
//! inputs) before ever invoking the real compiler binary. Bypass the
//! wrapper, as libclang necessarily does, and none of that injection
//! happens: not even `<algorithm>` resolves, because libc++'s headers live
//! in a directory nothing tells libclang about. Verified empirically in this
//! repo's nix devShell: parsing `nlohmann/json.hpp` (C++17, single header)
//! through raw libclang with no extra arguments fails with a **fatal**
//! `'algorithm' file not found` before a single declaration is visited.
//!
//! This is not specific to nix. Any environment where "the compiler on
//! `PATH`" is not literally the binary that owns the running process's
//! default search path (a wrapper, a `PATH` shim, a cross-compiler driver
//! with a baked-in sysroot different from the host's) has the same shape of
//! gap. `LIBCLANG_PATH` (see this crate's `Cargo.toml` / flake devShell)
//! solves the *analogous* "which libclang.dylib" problem for loading the
//! library at all; this module solves the same problem one layer up, for
//! *what that libclang needs to be told* once loaded.
//!
//! # How
//!
//! Ask a real compiler driver — the same kind of program a human would run
//! — what its own default search path is, via the standard, well-documented
//! `-v -E -x c++ -` incantation (verbose preprocessing of empty stdin; every
//! driver from GCC to Clang honours it), and parse the `#include <...>
//! search starts here:` / `End of search list.` block out of its stderr.
//! Whatever wrapper machinery the driver itself is built from — nix's
//! `cc-wrapper`, a vendor's `xcrun`, a plain unwrapped `clang` — is exactly
//! what already knows the right answer; this module does not hardcode a
//! single toolchain path itself, nix or otherwise, so it is not tied to this
//! machine's current nix store hashes.
//!
//! Framework directories (macOS `-F` search, printed with a `(framework
//! directory)` suffix by Clang) are skipped: they resolve `#include
//! <Foo/Foo.h>`-style Objective-C framework headers, which none of this
//! corpus's C/C++ fixtures use, and passing a framework path as `-isystem`
//! would be wrong (different lookup semantics).
//!
//! # Failure mode
//!
//! If no driver can be found or it produces no parseable search list, this
//! returns an empty `Vec` — [`super::producer::ClangProducer::invoke`] then
//! falls back to exactly the arguments it always used before this module
//! existed (bare `-std=`/`-x`). That is a real, silent capability loss (see
//! the doc comment on the call site), so a diagnostic is printed to stderr
//! either way; nothing here turns a real absence into a fabricated result.
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

/// Candidate driver executables, tried in order. `NUDOX_CLANG_DRIVER` lets a
/// caller pin an exact binary (e.g. the unwrapped compiler colocated with a
/// specific `LIBCLANG_PATH`, to guarantee the resource-dir libclang itself
/// resolves and the search list this module discovers agree); otherwise fall
/// back to whatever `clang`/`cc` resolve to on `PATH`.
fn candidate_drivers() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(pinned) = std::env::var("NUDOX_CLANG_DRIVER") {
        out.push(pinned);
    }
    out.push("clang".to_owned());
    out.push("cc".to_owned());
    out
}

/// The discovered system include directories, computed once per process.
/// Every `ClangProducer::invoke` call reuses this — the host toolchain does
/// not change mid-process, and spawning a compiler driver per translation
/// unit would be wasteful for a real package with hundreds of files.
static SYSTEM_INCLUDE_DIRS: OnceLock<Vec<PathBuf>> = OnceLock::new();

/// The `-isystem <dir>` argument pairs to prepend to every file parsed by
/// libclang, discovered from the host toolchain. Cached after first call.
pub(crate) fn args() -> Vec<String> {
    SYSTEM_INCLUDE_DIRS
        .get_or_init(discover)
        .iter()
        .flat_map(|dir| ["-isystem".to_owned(), dir.to_string_lossy().into_owned()])
        .collect()
}

fn discover() -> Vec<PathBuf> {
    for driver in candidate_drivers() {
        match run_driver_verbose(&driver) {
            Ok(stderr) => {
                let dirs = parse_search_list(&stderr);
                if !dirs.is_empty() {
                    return dirs;
                }
                eprintln!(
                    "nudox-languages: `{driver} -v -E -x c++ -` ran but produced no \
                     parseable '#include <...> search starts here:' block; trying next \
                     candidate"
                );
            }
            Err(e) => {
                eprintln!(
                    "nudox-languages: could not run `{driver}` to discover system include paths: {e}"
                );
            }
        }
    }
    eprintln!(
        "nudox-languages: no system include search path could be discovered from any of \
         {:?}; falling back to bare -std=/-x arguments only. Any file that includes a standard \
         library or SDK header will fail to resolve it and lower an empty/degraded oracle for \
         that translation unit, not an error — this is the gap system_includes exists to close. \
         Set NUDOX_CLANG_DRIVER to an explicit compiler path to fix.",
        candidate_drivers()
    );
    Vec::new()
}

/// Run `driver -v -E -x c++ -` against empty stdin and return stderr as a
/// `String`. `-E` (preprocess only, discard output) plus `-` (read from
/// stdin) means no real file is needed on disk; `-v` is what makes the
/// driver print its search list. stdout (the preprocessed, empty file) is
/// discarded; the search list is on stderr for every driver this was tested
/// against (Clang; GCC matches the same convention).
fn run_driver_verbose(driver: &str) -> std::io::Result<String> {
    use std::io::Write as _;
    use std::process::Stdio;

    let mut child = Command::new(driver)
        .args(["-v", "-E", "-x", "c++", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;

    // Empty stdin, then close it so the driver's preprocessor sees EOF
    // immediately instead of blocking.
    drop(child.stdin.take().map(|mut s| s.flush()));

    let output = child.wait_with_output()?;
    Ok(String::from_utf8_lossy(&output.stderr).into_owned())
}

/// Parse the directory list out of a `clang -v` (or `gcc -v`) stderr dump:
///
/// ```text
/// #include <...> search starts here:
///  /path/one
///  /path/two
///  /path/framework (framework directory)
/// End of search list.
/// ```
///
/// Only the angle-bracket (`<...>`) list is used — quote (`"..."`) search
/// adds nothing beyond it in every driver's own default output (quote search
/// is angle search plus the including file's own directory, which is not a
/// fixed system path to begin with).
fn parse_search_list(stderr: &str) -> Vec<PathBuf> {
    let mut in_list = false;
    let mut dirs = Vec::new();
    for line in stderr.lines() {
        if line.contains("#include <...> search starts here:") {
            in_list = true;
            continue;
        }
        if line.trim() == "End of search list." {
            break;
        }
        if !in_list {
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.ends_with("(framework directory)") {
            continue; // see module docs: -F semantics, not -isystem.
        }
        dirs.push(PathBuf::from(trimmed));
    }
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_representative_clang_verbose_dump() {
        let stderr = "\
clang version 21.1.8
Target: arm64-apple-darwin
Thread model: posix
 \"/usr/bin/clang\" -cc1 ...
clang -cc1 version 21.1.8
ignoring nonexistent directory \"/does/not/exist\"
#include \"...\" search starts here:
#include <...> search starts here:
 /nix/store/aaa-libcxx/include
 /nix/store/aaa-libcxx/include/c++/v1
 /nix/store/aaa-sdk/usr/include
 /nix/store/aaa-sdk/System/Library/Frameworks (framework directory)
End of search list.
# 1 \"<stdin>\"
";
        let dirs = parse_search_list(stderr);
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/nix/store/aaa-libcxx/include"),
                PathBuf::from("/nix/store/aaa-libcxx/include/c++/v1"),
                PathBuf::from("/nix/store/aaa-sdk/usr/include"),
            ],
            "framework directory must be excluded, angle-bracket list order preserved: {dirs:?}"
        );
    }

    #[test]
    fn missing_search_list_yields_empty_not_a_panic() {
        assert_eq!(
            parse_search_list("no useful output here"),
            Vec::<PathBuf>::new()
        );
    }

    #[test]
    fn real_host_driver_discovers_a_nonempty_search_list() {
        // This is the one test in this module that actually shells out. If
        // the sandbox this runs in has no `clang`/`cc` on PATH at all, this
        // would be a false failure unrelated to the parsing logic above —
        // but every environment this producer is meant to run in (the nix
        // devShell, any real dev machine) has one, and the whole point of
        // `discover()` is to prove it finds a nonempty list on such a
        // machine, not just that the string parser works on canned input.
        let dirs = discover();
        assert!(
            !dirs.is_empty(),
            "expected a nonempty system include search list from the host toolchain; \
             got none — see system_includes::discover's stderr diagnostics for which \
             driver(s) were tried"
        );
    }
}
