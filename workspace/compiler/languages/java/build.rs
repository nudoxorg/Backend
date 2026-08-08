//! Compiles the vendored javadoc doclet (`oracle/Extractor.java` +
//! `oracle/Json.java`) into `.class` files at *this crate's* build time.
//!
//! # Why a `build.rs`
//!
//! Three producers in this workspace (Go, C#, Java) are external-toolchain
//! subprocesses that emit a JSON document (see `nudox_producer::oracle`'s
//! module doc). Go's oracle is a separate `go build` the caller runs by hand
//! (see `GoProducer::invoke_oracle`'s doc comment and its `NUDOX_GO_ORACLE_BIN`
//! escape hatch) — there is no established "compile the foreign-language
//! oracle automatically" precedent anywhere else in this workspace to reuse.
//!
//! For Java specifically, hand-compilation is avoidable: `javac` is a
//! deterministic, hermetic, fast (< 1s) compile of two small files with zero
//! external dependencies (the doclet uses only JDK-shipped APIs —
//! `jdk.javadoc.doclet`, `com.sun.source.*`, `javax.lang.model.*`). Doing it
//! in `build.rs` means `cargo test -p nudox-producer-java` and
//! `cargo check -p nudox-producer-java` both produce a runnable oracle with no
//! manual pre-step, which the Go crate's contract does not offer. See
//! `src/producer.rs`'s module doc for the runtime side of this contract
//! (where the compiled classes are found, and how to override the location).
//!
//! # Toolchain
//!
//! Requires `javac` (JDK 17+ — the doclet uses `switch` pattern-matching
//! syntax) on `PATH`, or `NUDOX_JAVAC` pointing at one explicitly. This
//! program's environment provides JDK 21 via nix.

use std::{
    path::PathBuf,
    process::Command,
};

fn main() {
    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo"),
    );
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is set by cargo"));

    let extractor = manifest_dir.join("oracle").join("Extractor.java");
    let json = manifest_dir.join("oracle").join("Json.java");

    // Rebuild only when the doclet sources actually change.
    println!("cargo:rerun-if-changed={}", extractor.display());
    println!("cargo:rerun-if-changed={}", json.display());
    println!("cargo:rerun-if-env-changed=NUDOX_JAVAC");

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
        Ok(s) => panic!(
            "`{javac}` exited with {s} compiling the Java oracle doclet \
             ({} {}) — is a JDK 17+ javac on PATH (or NUDOX_JAVAC)?",
            extractor.display(),
            json.display(),
        ),
        Err(e) => panic!(
            "failed to spawn `{javac}` to compile the Java oracle doclet: {e}. \
             Set NUDOX_JAVAC to an explicit javac binary, or put a JDK 17+ on PATH."
        ),
    }
}
