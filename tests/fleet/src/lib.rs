//! Typed lane contract shared by the fleet toolchain runner and its callers.
//!
//! The executable runner lives beside this crate at `tests/fleet/run-fleet.sh`.
//! This module is the single Rust source of truth for the seven
//! compiler-corpus lanes and the semantic-authority variables that runner must
//! verify. Keeping the contract typed lets other crates consume the lane
//! vocabulary without parsing shell, and the unit tests below pin the exact
//! variable wiring the corpus tests expect.

/// One native-toolchain lane in the seven-language compiler corpus.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Lane {
    /// Stable lane name, matching `CorpusLanguage` spelling in lower case.
    pub name: &'static str,
    /// Native tool whose toolchain row the corpus test probes.
    pub tool: &'static str,
    /// Compiler-authority environment variable the corpus test consults.
    pub variable: &'static str,
    /// Package-cache root environment variable for the real-package pass.
    pub corpus_variable: &'static str,
}

/// All lanes in `CorpusLanguage::ALL` order.
pub const LANES: [Lane; 7] = [
    Lane {
        name: "rust",
        tool: "rustc",
        variable: "COMPILER_STABLE_TOOLCHAIN",
        corpus_variable: "NUDOX_RUST_CORPUS_DIR",
    },
    Lane {
        name: "typescript",
        tool: "tsc",
        variable: "COMPILER_TYPESCRIPT_COMPILER",
        corpus_variable: "NUDOX_TYPESCRIPT_CORPUS_DIR",
    },
    Lane {
        name: "python",
        tool: "python3",
        variable: "COMPILER_PYTHON_COMPILER",
        corpus_variable: "NUDOX_PYTHON_CORPUS_DIR",
    },
    Lane {
        name: "go",
        tool: "go",
        variable: "COMPILER_GO_COMPILER",
        corpus_variable: "NUDOX_GO_CORPUS_DIR",
    },
    Lane {
        name: "java",
        tool: "javac",
        variable: "COMPILER_JAVA_COMPILER",
        corpus_variable: "NUDOX_JAVA_CORPUS_DIR",
    },
    Lane {
        name: "csharp",
        tool: "dotnet",
        variable: "COMPILER_CSHARP_COMPILER",
        corpus_variable: "NUDOX_CSHARP_CORPUS_DIR",
    },
    Lane {
        name: "clang",
        tool: "clang",
        variable: "COMPILER_CLANG_COMPILER",
        corpus_variable: "NUDOX_CLANG_CORPUS_DIR",
    },
];

/// The lane contract in `CorpusLanguage::ALL` order.
#[must_use]
pub const fn lanes() -> &'static [Lane; 7] {
    &LANES
}

/// Semantic-authority variables the runner verifies in addition to the seven
/// compiler lanes. `RUSTC` and `LIBCLANG_PATH` are Rust-native seams; the
/// `NUDOX_*` names are the producers' explicit overrides.
pub const AUTHORITY_VARIABLES: [&str; 8] = [
    "RUSTC",
    "LIBCLANG_PATH",
    "NUDOX_JDK",
    "NUDOX_TYPESCRIPT_CHECKER_BIN",
    "NUDOX_CSHARP_DOTNET",
    "NUDOX_PYREFLY_BIN",
    "NUDOX_CLANG_DRIVER",
    "NUDOX_GO_ORACLE_BIN",
];

/// The command that drives the deterministic 200-row, seven-lane selection.
pub const CORPUS_COMMAND: &str =
    "cargo test -p backend-flow --test compiler_corpus -- --nocapture --test-threads=1";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lanes_cover_every_language_exactly_once() {
        let mut names: Vec<&str> = lanes().iter().map(|lane| lane.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 7, "lane names must be unique");
        for lane in lanes() {
            assert!(!lane.tool.is_empty(), "{} needs a probe tool", lane.name);
            assert!(
                lane.variable.starts_with("COMPILER_"),
                "{} compiler variable must be a COMPILER_ authority",
                lane.name
            );
            assert!(
                lane.corpus_variable.starts_with("NUDOX_")
                    && lane.corpus_variable.ends_with("_CORPUS_DIR"),
                "{} corpus root must be a NUDOX_*_CORPUS_DIR",
                lane.name
            );
        }
    }

    #[test]
    fn authority_variables_are_unique_and_closed() {
        let mut variables = AUTHORITY_VARIABLES;
        variables.sort_unstable();
        for pair in variables.windows(2) {
            assert_ne!(pair[0], pair[1], "authority variables must be unique");
        }
        for required in [
            "RUSTC",
            "NUDOX_JDK",
            "NUDOX_TYPESCRIPT_CHECKER_BIN",
            "NUDOX_CSHARP_DOTNET",
            "NUDOX_PYREFLY_BIN",
            "NUDOX_CLANG_DRIVER",
            "NUDOX_GO_ORACLE_BIN",
        ] {
            assert!(
                variables.contains(&required),
                "authority contract is missing {required}"
            );
        }
    }
}
