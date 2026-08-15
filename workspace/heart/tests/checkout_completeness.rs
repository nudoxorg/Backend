//! Every source file the build needs is actually *in* the repository.
//!
//! # Why this file exists
//!
//! On 2026-08-08 a clean checkout of `canonical` could not compile. Six files
//! were `mod`-declared from tracked modules and had never been `git add`ed —
//! `clang/src/compile_commands.rs`, `clang/src/system_includes.rs`,
//! `csharp/src/producer.rs`, `java/src/producer.rs`,
//! `gui/src/views/command_overlay.rs`, `index/transport/mod.rs`. Four workspace
//! crates plus `lindsey` failed at module resolution, before a single test could
//! run. `workspace/compiler/languages/java/build.rs` was missing too, which is
//! the same defect wearing a worse disguise: cargo *autodetects* `build.rs` with
//! no manifest key pointing at it, so its absence is not an error — the javadoc
//! doclet simply never compiles and the Java oracle silently ceases to exist.
//!
//! None of them were ignored. `git check-ignore -v` reported the `!/**/*.rs`
//! negation for every one: the `.gitignore` allowlist was right and nobody ran
//! `git add`.
//!
//! # Why no existing check could have caught it
//!
//! This is the defect class that is *structurally invisible to the machine that
//! has the bug*. Every check we run — `cargo check --workspace --all-targets`,
//! the nextest gate, every `#[test]` in this repository — runs against the
//! working tree, and the working tree has the files. They pass here and fail for
//! everyone else. The only observer that can see the difference is one that asks
//! git what it would hand a stranger, which is what this test does.
//!
//! docs/ISSUES.md had already recorded the symptom ("~27 untracked test .rs files …
//! a clean checkout of HEAD could not reproduce most of this document's green
//! rows") and it was filed under housekeeping. It was not housekeeping: it was
//! a total build failure for every reader of the repository, and it survived
//! because the register described it in prose instead of asserting it.
//!
//! # Scope
//!
//! Three properties, all about *packaging*, not about code:
//!
//! 1. every `mod NAME;` in a tracked `.rs` resolves to a file git also tracks;
//! 2. every `build.rs` sitting beside a `Cargo.toml` is tracked, because cargo
//!    runs it on presence alone and reports nothing when it is absent;
//! 3. every integration test under a `tests/` directory is tracked, for exactly
//!    the same reason — cargo *autodiscovers* `tests/*.rs`, so an untracked one
//!    runs here and does not exist anywhere else.
//!
//! The third is the subtlest and has the longest history in this repository.
//! docs/LIMITATIONS.md L48 records the mirror-image bug: cargo's autodiscovery globs
//! only `tests/*.rs` and `tests/*/main.rs`, so 18 files sitting at
//! `tests/<dir>/<name>.rs` had no target, never compiled, and cargo said
//! nothing. This is the same silence from the other side — the file is
//! discovered and run *locally*, and is simply absent for everyone else. Both
//! directions produce a suite whose size depends on who is looking at it.
//!
//! Untracked files that nothing declares and nothing discovers are deliberately
//! *not* failures. Scratch files, experiments and work in progress are normal; a
//! file the build reaches for and cannot find is not.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Repository root — this crate is `workspace/heart`, so up two.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace/heart has two ancestors")
        .to_path_buf()
}

/// Ask git for its index. `git ls-files` and not `std::fs`: the entire point is
/// to see the repository as a stranger receives it, and only git knows that.
fn tracked(root: &Path, pattern: &str) -> Vec<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", pattern])
        .output()
        .expect("run `git ls-files` — this test requires a git checkout");

    assert!(
        out.status.success(),
        "`git ls-files {pattern}` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    String::from_utf8(out.stdout)
        .expect("git paths are UTF-8 in this repository")
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The module names a Rust source declares as *separate files* (`mod name;`).
///
/// Only the terminating-semicolon form matters. `mod name { … }` is inline and
/// needs no file, so it must not be reported — that would be a false positive on
/// every `#[cfg(test)] mod tests { … }` in the workspace and the test would be
/// deleted within a day.
///
/// `#[path = "…"] mod name;` is not handled: it would be reported as missing
/// under its default path. That is a false *positive* — loud, and naming the
/// exact declaration — rather than a silent miss, which is the correct direction
/// for a check whose entire purpose is to not be quietly satisfiable. There are
/// no `#[path]` module declarations in this workspace today; if one is added,
/// this comment is the instruction for how to extend the parser.
fn declared_modules(source: &str) -> Vec<String> {
    let mut names = Vec::new();

    for raw in source.lines() {
        let line = raw.trim();
        if line.starts_with("//") {
            continue;
        }
        // Strip visibility: `pub`, `pub(crate)`, `pub(super)`, `pub(in …)`.
        let rest = if let Some(after) = line.strip_prefix("pub") {
            match after.strip_prefix('(') {
                Some(paren) => match paren.split_once(')') {
                    Some((_, tail)) => tail.trim_start(),
                    None => continue,
                },
                None => after.trim_start(),
            }
        } else {
            line
        };

        let Some(after_mod) = rest.strip_prefix("mod ") else {
            continue;
        };
        let Some(name) = after_mod.trim().strip_suffix(';') else {
            continue; // inline `mod name { … }`, or a `mod` in a longer expression
        };
        let name = name.trim();
        if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            names.push(name.to_owned());
        }
    }

    names
}

/// Where `mod name;` inside `source` looks for its file: `dir/name.rs`, or
/// `dir/name/mod.rs`. `dir` is the file's own directory for a crate root or a
/// `mod.rs`, and the sibling directory named after the file otherwise.
fn candidate_paths(source: &Path, module: &str) -> [PathBuf; 2] {
    let parent = source.parent().unwrap_or_else(|| Path::new(""));
    let stem = source.file_stem().and_then(|s| s.to_str()).unwrap_or("");

    let base = if matches!(stem, "lib" | "main" | "mod") {
        parent.to_path_buf()
    } else {
        parent.join(stem)
    };

    [base.join(format!("{module}.rs")), base.join(module).join("mod.rs")]
}

#[test]
fn every_declared_module_is_a_tracked_file() {
    let root = repo_root();
    let sources = tracked(&root, "*.rs");

    // A `git ls-files` that returned nothing — wrong directory, no checkout —
    // would make this test pass by scanning zero files. That is the exact
    // "green because it never ran" failure the suite exists to catch.
    assert!(
        sources.len() > 500,
        "expected the workspace to track many .rs files, found {} — this test \
         scanned nothing and would pass vacuously",
        sources.len()
    );

    let known: HashSet<&str> = sources.iter().map(String::as_str).collect();
    let mut missing = Vec::new();

    for relative in &sources {
        let absolute = root.join(relative);
        let Ok(text) = std::fs::read_to_string(&absolute) else {
            // Tracked but not on disk: a deletion staged but not committed. Not
            // this test's subject.
            continue;
        };

        for module in declared_modules(&text) {
            let candidates = candidate_paths(Path::new(relative), &module);

            // Resolved if git tracks either spelling.
            if candidates
                .iter()
                .any(|c| known.contains(c.to_string_lossy().as_ref()))
            {
                continue;
            }

            // Not tracked. Report only if the file is really there — that is the
            // "works for me, broken for you" case. A declaration pointing at
            // nothing at all is a plain compile error that every build already
            // catches, and is not worth a second, noisier report.
            if let Some(on_disk) = candidates.iter().find(|c| root.join(c).is_file()) {
                missing.push(format!(
                    "  {} — declared `mod {module};` in {relative}",
                    on_disk.display()
                ));
            }
        }
    }

    assert!(
        missing.is_empty(),
        "These files exist in this working tree and are NOT in git, so a clean \
         checkout cannot compile — the module declaration resolves to nothing:\n\
         {}\n\n\
         Every build check we run passes here, because the working tree has the \
         files; the failure lands only on someone who clones. Fix with `git add` \
         on each path above. If a file is genuinely meant to be absent, delete \
         its `mod` declaration in the same commit.",
        missing.join("\n")
    );
}

/// Cargo autodiscovers integration tests; an untracked one is a test that runs
/// for its author and does not exist for anyone else.
///
/// Scoped to the two shapes cargo actually globs — `tests/*.rs` and
/// `tests/*/main.rs` — because those are the paths that become targets without
/// a `[[test]]` stanza. A file at `tests/<dir>/<name>.rs` is *not* discovered
/// (docs/LIMITATIONS.md L48), so its absence from git changes nothing about what
/// runs, and reporting it here would be noise.
/// Every `include_str!`/`include_bytes!` target is tracked.
///
/// The third shape of the same defect, and the one with the most scar tissue in
/// this repository. `include_*!` resolves relative to the containing file and is
/// a *compile-time* read: if the target is missing the crate does not build at
/// all. `workspace/index/ecosystem/cpp/alias.rs:65` pulls in a 66 KB
/// `cpp_alias_seed.ron` this way, and `.gitignore:200` already carried a comment
/// explaining that without it "the whole `index` crate fails to compile" — the
/// allow-list rule was written and the file was still never added.
///
/// docs/LIMITATIONS.md L7 is the same story with a worse ending: `japanese.rs`
/// `include_bytes!`s a Vaporetto model, and what was on disk was a 4-byte file
/// containing the ASCII text `STUB`. That compiled cleanly and panicked on first
/// use. This test cannot judge *contents* — a stub of the right name still
/// passes here — so it is not a substitute for the checksum `japanese.rs`
/// declares. It only guarantees the file a stranger receives is the file the
/// author had.
#[test]
fn every_compile_time_include_is_tracked() {
    let root = repo_root();
    let sources = tracked(&root, "*.rs");
    // Every tracked path, not just the Rust ones — include targets are .ron,
    // .graphql, .trustfall, .model, … and scoping this to `*.rs` would report
    // every one of them as missing.
    let all_tracked: Vec<String> = tracked(&root, ".");
    let known: HashSet<&str> = all_tracked.iter().map(String::as_str).collect();
    let mut missing = Vec::new();

    for relative in &sources {
        let Ok(text) = std::fs::read_to_string(root.join(relative)) else {
            continue;
        };
        let dir = Path::new(relative).parent().unwrap_or_else(|| Path::new(""));

        for macro_name in ["include_str!", "include_bytes!"] {
            for (offset, _) in text.match_indices(macro_name) {
                let after = &text[offset + macro_name.len()..];
                // `include_str!("path")` — take the first string literal. A
                // non-literal argument (a `concat!`, an env expansion) is skipped
                // rather than guessed at; guessing would produce false positives
                // that get this test deleted.
                let Some(open) = after.find('"') else { continue };
                if after[..open].chars().any(|c| !"( \t\r\n".contains(c)) {
                    continue;
                }
                let Some(close) = after[open + 1..].find('"') else {
                    continue;
                };
                let literal = &after[open + 1..open + 1 + close];
                if literal.contains('$') || literal.is_empty() {
                    continue;
                }

                // Normalise `../` and `./` against the including file's dir.
                let dir_text = dir.to_string_lossy().into_owned();
                let mut parts: Vec<&str> = Vec::new();
                for segment in dir_text
                    .split('/')
                    .chain(literal.split('/'))
                    .filter(|s| !s.is_empty() && *s != ".")
                {
                    if segment == ".." {
                        parts.pop();
                    } else {
                        parts.push(segment);
                    }
                }
                let resolved = parts.join("/");

                if !known.contains(resolved.as_str()) && root.join(&resolved).is_file() {
                    missing.push(format!("  {resolved} — included by {relative}"));
                }
            }
        }
    }

    missing.sort();
    missing.dedup();
    assert!(
        missing.is_empty(),
        "These files are read at COMPILE TIME by include_str!/include_bytes! and \
         are NOT in git:\n{}\n\n\
         The including crate does not build without them, so this is a hard \
         build failure for everyone who clones — not a degraded mode. `git add` \
         each path, and if the blanket `/**/*` rule in .gitignore swallows it, \
         add the allow-list negation in the same commit (writing the negation \
         alone is not enough — that is exactly how cpp_alias_seed.ron stayed \
         missing while .gitignore carried a comment explaining why it must not).",
        missing.join("\n")
    );
}

#[test]
fn every_autodiscovered_integration_test_is_tracked() {
    let root = repo_root();
    let manifests = tracked(&root, "*Cargo.toml");
    let known: HashSet<String> = tracked(&root, "*.rs").into_iter().collect();
    let mut missing = Vec::new();

    for manifest in &manifests {
        let tests_dir = Path::new(manifest)
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join("tests");
        let Ok(entries) = std::fs::read_dir(root.join(&tests_dir)) else {
            continue;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let candidate = if path.is_file() && path.extension().is_some_and(|e| e == "rs") {
                tests_dir.join(path.file_name().expect("read_dir yields named entries"))
            } else if path.is_dir() && path.join("main.rs").is_file() {
                tests_dir
                    .join(path.file_name().expect("read_dir yields named entries"))
                    .join("main.rs")
            } else {
                continue;
            };

            if !known.contains(candidate.to_string_lossy().as_ref()) {
                missing.push(format!("  {}", candidate.display()));
            }
        }
    }

    missing.sort();
    assert!(
        missing.is_empty(),
        "These integration tests exist here and are NOT in git:\n{}\n\n\
         Cargo autodiscovers `tests/*.rs` and `tests/*/main.rs`, so each of \
         these compiles and runs on this machine and does not exist for anyone \
         who clones. The suite's size then depends on who is looking at it, and \
         a green run here proves nothing about a green run there — which is how \
         most of docs/ISSUES.md's evidence came to rest on files that were never \
         committed. `git add` each path, or move the file out of `tests/` if it \
         is genuinely scratch work.",
        missing.join("\n")
    );
}

#[test]
fn every_build_script_is_tracked() {
    let root = repo_root();
    let manifests = tracked(&root, "*Cargo.toml");

    assert!(
        manifests.len() > 10,
        "expected many tracked Cargo.toml files, found {} — scanned nothing",
        manifests.len()
    );

    let known: HashSet<String> = tracked(&root, "*build.rs").into_iter().collect();
    let mut missing = Vec::new();

    for manifest in &manifests {
        let dir = Path::new(manifest).parent().unwrap_or_else(|| Path::new(""));
        let script = dir.join("build.rs");
        if root.join(&script).is_file() && !known.contains(script.to_string_lossy().as_ref()) {
            missing.push(format!("  {}", script.display()));
        }
    }

    assert!(
        missing.is_empty(),
        "These build scripts exist here and are NOT in git:\n{}\n\n\
         This is worse than a missing module, not better. Cargo autodetects \
         `build.rs` by presence, with no manifest key pointing at it, so an \
         absent one is not an error — the build simply, silently, does not run \
         that step. `workspace/compiler/languages/java/build.rs` compiles the \
         javadoc doclet; without it the Java oracle does not exist and the \
         producer degrades with no diagnostic. That is the same failure shape as \
         docs/LIMITATIONS.md L50, arrived at through packaging instead of code.",
        missing.join("\n")
    );
}
