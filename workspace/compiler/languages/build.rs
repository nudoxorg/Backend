//! Compiles the vendored javadoc doclet (`oracle/java/Extractor.java` +
//! `oracle/java/Json.java`) into `.class` files at *this crate's* build time.
//!
//! # Why a `build.rs`
//!
//! Three producers in this workspace (Go, C#, Java) are external-toolchain
//! subprocesses that emit a JSON document (see `crate::oracle`'s
//! module doc). Go's oracle is a separate `go build` the caller runs by hand
//! (see `GoProducer::invoke_oracle`'s doc comment and its `NUDOX_GO_ORACLE_BIN`
//! escape hatch) — there is no established "compile the foreign-language
//! oracle automatically" precedent anywhere else in this workspace to reuse.
//!
//! For Java specifically, hand-compilation is avoidable: `javac` is a
//! deterministic, hermetic, fast (< 1s) compile of two small files with zero
//! external dependencies (the doclet uses only JDK-shipped APIs —
//! `jdk.javadoc.doclet`, `com.sun.source.*`, `javax.lang.model.*`). Doing it
//! in `build.rs` means `cargo test -p nudox-languages` and
//! `cargo check -p nudox-languages` both produce a runnable oracle with no
//! manual pre-step, which the Go crate's contract does not offer. See
//! `src/java/invoke.rs`'s module doc for the runtime side of this contract
//! (where the compiled classes are found, and how to override the location).
//!
//! # Toolchain
//!
//! Requires `javac` (JDK 17+ — the doclet uses `switch` pattern-matching
//! syntax) on `PATH`, or `NUDOX_JAVAC` pointing at one explicitly. This
//! program's environment provides JDK 21 via nix.
//!
//! A second, isolated Java 8 doclet is compiled when `NUDOX_JAVA8_JAVAC` is
//! provided. It exists only for source sets such as Lombok 1.18.30 whose
//! compiler-internal APIs predate JPMS; the normal oracle remains unchanged.
//!
//! # No-JDK hosts (including Windows without a JDK)
//!
//! The doclet is a *runtime* aid for exactly one producer (Java), not a
//! requirement of the crate itself. On a host with no `javac` the build does
//! **not** fail: it emits `cfg(nudox_java_oracle_unavailable)` and continues,
//! and the Java producer then reports a typed `ToolchainMissing`-shaped
//! `OracleSpawn` at invoke time instead of crashing the build. This is what
//! keeps a GUI build — which only needs the *crate* to compile, not the Java
//! producer to run — possible on a machine that never installed a JDK.

use std::{path::PathBuf, process::Command};

fn main() {
    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo"),
    );
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is set by cargo"));

    let extractor = manifest_dir
        .join("oracle")
        .join("java")
        .join("Extractor.java");
    let json = manifest_dir.join("oracle").join("java").join("Json.java");

    // Rebuild only when the doclet sources actually change.
    println!("cargo:rerun-if-changed={}", extractor.display());
    println!("cargo:rerun-if-changed={}", json.display());
    println!("cargo:rerun-if-env-changed=NUDOX_JAVAC");
    println!("cargo:rerun-if-env-changed=NUDOX_JAVA8_JAVAC");

    // Declare the fallback cfg so `cfg!(nudox_java_oracle_unavailable)` in
    // `src/java/invoke.rs` never trips the `unexpected_cfgs` lint.
    println!("cargo::rustc-check-cfg=cfg(nudox_java_oracle_unavailable)");

    let classes_dir = out_dir.join("classes");
    std::fs::create_dir_all(&classes_dir).unwrap_or_else(|e| {
        panic!(
            "failed to create oracle classes directory {}: {e}",
            classes_dir.display()
        )
    });

    let javac = std::env::var("NUDOX_JAVAC").unwrap_or_else(|_| "javac".to_owned());

    let status = Command::new(&javac)
        .arg("-d")
        .arg(&classes_dir)
        .arg("-encoding")
        .arg("UTF-8")
        .arg(&extractor)
        .arg(&json)
        .status();

    match status {
        Ok(s) if s.success() => {}
        Ok(s) => degrade(
            &javac,
            &format!(
                "`{javac}` exited with {s} compiling the Java oracle doclet \
             ({} {}); the Java producer will be unavailable",
                extractor.display(),
                json.display(),
            ),
        ),
        Err(e) => degrade(
            &javac,
            &format!(
                "failed to spawn `{javac}` to compile the Java oracle doclet: {e}; \
             the Java producer will be unavailable",
            ),
        ),
    }

    let legacy = manifest_dir
        .join("oracle")
        .join("java8")
        .join("LegacyExtractor.java");
    println!("cargo:rerun-if-changed={}", legacy.display());
    if let Ok(java8_javac) = std::env::var("NUDOX_JAVA8_JAVAC") {
        let legacy_dir = out_dir.join("java8-classes");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        let legacy_status = Command::new(&java8_javac)
            .arg("-source")
            .arg("8")
            .arg("-target")
            .arg("8")
            .arg("-classpath")
            .arg(
                PathBuf::from(&java8_javac)
                    .parent()
                    .and_then(|p| p.parent())
                    .map_or_else(|| PathBuf::from("tools.jar"), |p| p.join("lib/tools.jar")),
            )
            .arg("-d")
            .arg(&legacy_dir)
            .arg("-encoding")
            .arg("UTF-8")
            .arg(&legacy)
            .status();
        match legacy_status {
            Ok(s) if s.success() => {}
            Ok(s) => println!(
                "cargo:warning=`{java8_javac}` exited with {s} compiling the Java 8 Lombok oracle"
            ),
            Err(e) => println!("cargo:warning:failed to spawn Java 8 javac `{java8_javac}`: {e}"),
        }
    }
}

/// Record the no-`javac` state and continue the build instead of failing it.
///
/// The crate still compiles; only the Java producer is degraded, and it says
/// so with a typed error rather than a missing-classes failure at runtime.
fn degrade(_javac: &str, detail: &str) {
    println!("cargo:warning={detail}");
    println!("cargo:rustc-cfg=nudox_java_oracle_unavailable");
}
