//! Proves the borrowed rust-analyzer authority transaction against a real Cargo package.
//! Exercises inferred types, static impl dispatch, generic substitutions, macro provenance, and spans.
//! Rejects edition drift and distinguishes foreign module definitions from local source coordinates.

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use compiler_languages_rust::{
    RustAnalysisControl, RustAuthorityError, RustDefinition, RustProject, RustToolchain,
    SemanticKind, SourceByteLimit, SourceOrigin,
};
use compiler_vocabulary::RustEdition;
use ra_ap_syntax::AstNode;

/// Separates concurrently executing fixtures created during one process lifetime.
static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Builds a Cargo fixture that requires both local and sibling-module authority facts.
fn project_root() -> Result<PathBuf, TestFailure> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(TestFailure::Clock)?
        .as_nanos();
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("nudox-rust-authority-{nonce}-{sequence}"));
    fs::create_dir_all(root.join("src")).map_err(|source| TestFailure::Io {
        operation: "create fixture",
        source,
    })?;
    write_fixture(
        root.join("Cargo.toml"),
        "[package]\nname = \"authority_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        "write manifest",
    )?;
    write_fixture(
        root.join("src/lib.rs"),
        "//! Démonstrates borrowed Rust semantic authority.\n\nmod sibling;\n\n/// Describes a generic service.\npub trait Service<T: Clone> {\n    type Output: Clone;\n\n    /// Computes an output.\n    fn run(&self, input: T) -> Option<Self::Output>;\n}\n\npub struct Capsule {\n    pub payload: Vec<Option<String>>,\n}\n\npub enum Event { Message(String) }\n\npub struct Worker;\n\nimpl Service<String> for Worker {\n    type Output = String;\n\n    fn run(&self, input: String) -> Option<String> {\n        Some(input)\n    }\n}\n\nmacro_rules! invoke_once { ($value:expr) => { $value }; }\n\npub fn echo<T: Clone>(value: T) -> T { value }\n\npub fn direct(worker: &Worker, value: String) -> Option<String> {\n    worker.run(value)\n}\n\npub fn foreign(value: Option<String>) -> Option<String> {\n    sibling::assist(value)\n}\n\npub fn generic(value: String) -> String { echo::<String>(value) }\n\npub fn invoke(worker: &Worker, value: String) -> Option<String> {\n    invoke_once!(sibling::assist(worker.run(value)))\n}\n\n@\n",
        "write crate root",
    )?;
    write_fixture(
        root.join("src/sibling.rs"),
        "pub fn assist(value: Option<String>) -> Option<String> { value }\n",
        "write sibling module",
    )?;
    Ok(root)
}

/// Writes one fixture file while preserving the operating-system failure cause.
fn write_fixture(
    path: PathBuf,
    contents: &str,
    operation: &'static str,
) -> Result<(), TestFailure> {
    fs::write(path, contents).map_err(|source| TestFailure::Io { operation, source })
}

/// Proves that the authority never erases analyzer-owned semantic values before lowering.
#[test]
fn borrowed_authority_preserves_hir_types_resolution_macros_and_exact_spans()
-> Result<(), TestFailure> {
    let root = project_root()?;
    let outcome = (|| {
        let toolchain = RustToolchain::discover(rustc_path()).map_err(RustAuthorityError::from)?;
        let project = RustProject::open(&root, &toolchain, RustEdition::Rust2024)?;
        let running = AtomicBool::new(false);
        let control = RustAnalysisControl {
            cancelled: &running,
            maximum_source_bytes: SourceByteLimit::from(8_192),
            deadline: Instant::now() + std::time::Duration::from_secs(180),
        };
        project.analyze(control, |authority| {
            let root_span = authority.span(authority.root.syntax())?;
            if authority.source_at(root_span)? != authority.source {
                return Err(RustAuthorityError::UnresolvedInferredType);
            }
            let mut saw_exact_parse_error = false;
            authority.visit_syntax_diagnostics(|span| {
                saw_exact_parse_error |= usize::try_from(span.start)
                    .ok()
                    .and_then(|start| authority.source.get(start..))
                    .is_some_and(|suffix| suffix.starts_with(b"@\n"));
            });
            if !saw_exact_parse_error {
                return Err(RustAuthorityError::MissingSemanticFact {
                    fact: SemanticKind::Builtin,
                });
            }
            let module_found = authority.declarations().any(|declaration| {
                let Ok(span) = authority.span(&declaration.syntax) else {
                    return false;
                };
                if authority.source_at(span).ok() != Some(b"mod sibling;") {
                    return false;
                }
                let RustDefinition::Module(_) = declaration.definition else {
                    return false;
                };
                declaration.kind == SemanticKind::Module
            });
            if !module_found {
                return Err(RustAuthorityError::MissingSemanticFact {
                    fact: SemanticKind::Module,
                });
            }
            let trait_found = authority.declarations().any(|declaration| {
                let span = authority.span(&declaration.syntax).ok();
                let service = span
                    .and_then(|span| authority.source_at(span).ok())
                    .is_some_and(|source| {
                        source
                            .windows(b"trait Service".len())
                            .any(|window| window == b"trait Service")
                    });
                if service {
                    let mut docs = false;
                    authority.visit_documentation(&declaration.syntax, |documentation| {
                        docs |= documentation == "Describes a generic service.";
                    });
                    let RustDefinition::Trait(definition) = declaration.definition else {
                        return false;
                    };
                    let generic_parameters =
                        definition.type_or_const_param_count(authority.database, false);
                    let generic_bounds = ra_ap_hir::GenericDef::Trait(definition)
                        .type_or_const_params(authority.database)
                        .iter()
                        .filter_map(|parameter| parameter.as_type_param(authority.database))
                        .any(|parameter| !parameter.trait_bounds(authority.database).is_empty());
                    let associated_output = definition
                        .items(authority.database)
                        .into_iter()
                        .any(|item| matches!(item, ra_ap_hir::AssocItem::TypeAlias(_)));
                    return docs
                        && declaration.kind == SemanticKind::Trait
                        && generic_parameters >= 1
                        && generic_bounds
                        && associated_output;
                }
                false
            });
            if !trait_found {
                return Err(RustAuthorityError::MissingSemanticFact {
                    fact: SemanticKind::Trait,
                });
            }
            let implementation_type = authority.declarations().find_map(|declaration| {
                (declaration.kind == SemanticKind::Implementation)
                    .then(|| declaration.definition.semantic_type(authority.database))
                    .flatten()
            });
            if implementation_type.is_none_or(|semantic_type| semantic_type.is_unknown()) {
                return Err(RustAuthorityError::UnresolvedInferredType);
            }
            let nested_field_type = authority.declarations().find_map(|declaration| {
                (declaration.kind == SemanticKind::Field)
                    .then(|| declaration.definition.semantic_type(authority.database))
                    .flatten()
            });
            if nested_field_type.is_none_or(|semantic_type| semantic_type.is_unknown()) {
                return Err(RustAuthorityError::UnresolvedInferredType);
            }
            let call = authority
                .method_calls()
                .find(|call| {
                    call.syntax
                        .name_ref()
                        .is_some_and(|name| name.text() == "run")
                })
                .ok_or(RustAuthorityError::MissingSemanticFact {
                    fact: SemanticKind::Function,
                })?;
            let inferred = call
                .inferred
                .ok_or(RustAuthorityError::MissingSemanticFact {
                    fact: SemanticKind::Function,
                })?;
            if inferred.original.is_unknown() {
                return Err(RustAuthorityError::UnresolvedInferredType);
            }
            let target = call.target.ok_or(RustAuthorityError::MissingSemanticFact {
                fact: SemanticKind::Function,
            })?;
            let SourceOrigin::Local(target_span) =
                authority.definition_origin(target, SemanticKind::Function)
            else {
                return Err(RustAuthorityError::MissingSemanticFact {
                    fact: SemanticKind::Implementation,
                });
            };
            if !authority
                .source_at(target_span)?
                .starts_with(b"fn run(&self, input: String)")
            {
                return Err(RustAuthorityError::UnresolvedInferredType);
            }
            let foreign_path = authority
                .paths()
                .find(|path| {
                    authority
                        .span(path.syntax())
                        .ok()
                        .and_then(|span| authority.source_at(span).ok())
                        == Some(b"sibling::assist")
                })
                .ok_or(RustAuthorityError::MissingSemanticFact {
                    fact: SemanticKind::Function,
                })?;
            let Some((
                ra_ap_hir::PathResolution::Def(ra_ap_hir::ModuleDef::Function(function)),
                _substitution,
            )) = authority.resolve_path(&foreign_path)
            else {
                return Err(RustAuthorityError::UnresolvedInferredType);
            };
            if authority.definition_origin(function, SemanticKind::Function)
                != SourceOrigin::Foreign(SemanticKind::Function)
            {
                return Err(RustAuthorityError::UnresolvedInferredType);
            }
            let generic_path = authority
                .paths()
                .find(|path| {
                    authority
                        .span(path.syntax())
                        .ok()
                        .and_then(|span| authority.source_at(span).ok())
                        .is_some_and(|source| source.starts_with(b"echo"))
                })
                .ok_or(RustAuthorityError::MissingSemanticFact {
                    fact: SemanticKind::GenericParameter,
                })?;
            if authority
                .resolve_path(&generic_path)
                .is_none_or(|(_resolution, substitution)| substitution.is_none())
            {
                return Err(RustAuthorityError::UnresolvedInferredType);
            }
            let macro_call = authority
                .macro_calls()
                .find(|call| {
                    authority
                        .span(call.syntax())
                        .ok()
                        .and_then(|span| authority.source_at(span).ok())
                        .is_some_and(|source| source.starts_with(b"invoke_once!"))
                })
                .ok_or(RustAuthorityError::MissingSemanticFact {
                    fact: SemanticKind::Macro,
                })?;
            let macro_definition = authority.resolve_macro(&macro_call).ok_or(
                RustAuthorityError::MissingSemanticFact {
                    fact: SemanticKind::Macro,
                },
            )?;
            let SourceOrigin::Local(macro_definition_span) =
                authority.definition_origin(macro_definition, SemanticKind::Macro)
            else {
                return Err(RustAuthorityError::UnresolvedInferredType);
            };
            if !authority
                .source_at(macro_definition_span)?
                .starts_with(b"macro_rules! invoke_once")
            {
                return Err(RustAuthorityError::UnresolvedInferredType);
            }
            Ok(())
        })?;
        match RustProject::open(&root, &toolchain, RustEdition::Rust2021)?
            .analyze(control, |_| Ok(()))
        {
            Err(RustAuthorityError::EditionMismatch {
                requested: RustEdition::Rust2021,
                observed: RustEdition::Rust2024,
            }) => Ok(()),
            Ok(()) | Err(_) => Err(TestFailure::ProfileAuthority),
        }
    })();
    fs::remove_dir_all(&root).map_err(|source| TestFailure::Io {
        operation: "remove fixture",
        source,
    })?;
    outcome
}

/// Proves oversized input is rejected before Cargo workspace loading allocates project state.
#[test]
fn source_budget_rejects_before_workspace_loading() -> Result<(), TestFailure> {
    let root = project_root()?;
    let outcome = (|| {
        let toolchain = RustToolchain::discover(rustc_path()).map_err(RustAuthorityError::from)?;
        let project = RustProject::open(&root, &toolchain, RustEdition::Rust2024)?;
        let running = AtomicBool::new(false);
        match project.analyze(
            RustAnalysisControl {
                cancelled: &running,
                maximum_source_bytes: SourceByteLimit::from(1),
                deadline: Instant::now() + std::time::Duration::from_secs(180),
            },
            |_| Ok(()),
        ) {
            Err(RustAuthorityError::SourceBudget { actual, maximum })
                if actual > u64::from(*maximum) =>
            {
                Ok(())
            }
            Ok(()) | Err(_) => Err(TestFailure::SourceBudgetAuthority),
        }
    })();
    fs::remove_dir_all(&root).map_err(|source| TestFailure::Io {
        operation: "remove fixture",
        source,
    })?;
    outcome
}

/// Proves a pre-expired authority deadline returns its distinct typed terminal at entry.
#[test]
fn expired_authority_deadline_rejects_before_workspace_loading() -> Result<(), TestFailure> {
    let root = project_root()?;
    let outcome = (|| {
        let toolchain = RustToolchain::discover(rustc_path()).map_err(RustAuthorityError::from)?;
        let project = RustProject::open(&root, &toolchain, RustEdition::Rust2024)?;
        let cancelled = AtomicBool::new(false);
        match project.analyze(
            RustAnalysisControl {
                cancelled: &cancelled,
                maximum_source_bytes: SourceByteLimit::from(u32::MAX),
                deadline: Instant::now(),
            },
            |_| Ok(()),
        ) {
            Err(RustAuthorityError::DeadlineExceeded) => Ok(()),
            Ok(()) => Err(TestFailure::DeadlineAuthority),
            Err(error) => Err(TestFailure::Authority(error)),
        }
    })();
    fs::remove_dir_all(&root).map_err(|source| TestFailure::Io {
        operation: "remove fixture",
        source,
    })?;
    outcome
}

/// Proves a released permit cancels before filesystem or Cargo workspace work can begin.
#[test]
fn cancelled_authority_never_loads_the_workspace() -> Result<(), TestFailure> {
    let root = project_root()?;
    let outcome = (|| {
        let toolchain = RustToolchain::discover(rustc_path()).map_err(RustAuthorityError::from)?;
        let project = RustProject::open(&root, &toolchain, RustEdition::Rust2024)?;
        let cancelled = AtomicBool::new(true);
        match project.analyze(
            RustAnalysisControl {
                cancelled: &cancelled,
                maximum_source_bytes: SourceByteLimit::from(1),
                deadline: Instant::now() + std::time::Duration::from_secs(180),
            },
            |_| Ok(()),
        ) {
            Err(RustAuthorityError::Cancelled) => Ok(()),
            Ok(()) | Err(_) => Err(TestFailure::CancellationAuthority),
        }
    })();
    fs::remove_dir_all(&root).map_err(|source| TestFailure::Io {
        operation: "remove fixture",
        source,
    })?;
    outcome
}

/// Selects the configured compiler or a standard system lookup without preserving ambient metadata.
fn rustc_path() -> PathBuf {
    std::env::var_os("RUSTC").map_or_else(|| PathBuf::from("rustc"), PathBuf::from)
}

/// Test-only failure that preserves each setup and semantic authority cause.
#[derive(Debug, thiserror::Error)]
enum TestFailure {
    /// The system clock unexpectedly preceded its epoch.
    #[error("system clock preceded its epoch: {0}")]
    Clock(std::time::SystemTimeError),
    /// Fixture filesystem action failed.
    #[error("{operation} failed: {source}")]
    Io {
        /// Exact fixture action.
        operation: &'static str,
        /// Original operating-system cause.
        #[source]
        source: std::io::Error,
    },
    /// Cargo's semantic edition did not reject a conflicting compile profile.
    #[error("Rust edition drift was not rejected by the authority")]
    ProfileAuthority,
    /// Cancellation failed to stop before authority loading.
    #[error("cancelled Rust authority entered workspace loading")]
    CancellationAuthority,
    /// An expired Rust authority deadline was not returned as its exact typed terminal.
    #[error("expired Rust authority deadline was not rejected")]
    DeadlineAuthority,
    /// Root source admission failed to reject its declared byte-budget violation.
    #[error("Rust source budget admitted an oversized root before workspace loading")]
    SourceBudgetAuthority,
    /// Rust authority returned a typed failure.
    #[error("Rust authority failed: {0}")]
    Authority(#[from] RustAuthorityError),
}
