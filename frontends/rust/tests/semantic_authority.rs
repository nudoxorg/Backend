//! Proves the borrowed rust-analyzer authority transaction against a real Cargo package.
//! Exercises inferred types, static impl dispatch, generic substitutions, macro provenance, and spans.
//! Rejects edition drift and distinguishes foreign module definitions from local source coordinates.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use backend_frontend_rust::legacy::{
    RustAnalysisControl, RustAuthorityError, RustDefinition, RustFeatureControl, RustProject,
    RustSourceScope, RustToolchain, RustWorkspace, SemanticKind, SourceByteLimit, SourceOrigin,
};
use backend_semantic::vocabulary::RustEdition;
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
        "//! Démonstrates borrowed Rust semantic authority.\n\nmod sibling;\n\n/// Describes a generic service.\npub trait Service<T: Clone> {\n    type Output: Clone;\n\n    /// Computes an output.\n    fn run(&self, input: T) -> Option<Self::Output>;\n}\n\npub struct Capsule {\n    pub payload: Vec<Option<String>>,\n}\n\npub enum Event { Message(String) }\n\npub struct Worker;\n\nimpl Service<String> for Worker {\n    type Output = String;\n\n    fn run(&self, input: String) -> Option<String> {\n        Some(input)\n    }\n}\n\nmacro_rules! invoke_once { ($value:expr) => { $value }; }\n\npub fn echo<T: Clone>(value: T) -> T { value }\n\npub fn sweep<T: Clone>(x: T) -> T { x }\n\npub fn direct(worker: &Worker, value: String) -> Option<String> {\n    worker.run(value)\n}\n\npub fn foreign(value: Option<String>) -> Option<String> {\n    sibling::assist(value)\n}\n\npub fn generic(value: String) -> String { echo::<String>(value) }\n\npub fn invoke(worker: &Worker, value: String) -> Option<String> {\n    invoke_once!(sibling::assist(worker.run(value)))\n}\n\n@\n",
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
            if authority.source_scope != RustSourceScope::CargoTargetRoot {
                return Err(RustAuthorityError::MissingSemanticFact {
                    fact: SemanticKind::Module,
                });
            }
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
                    if authority
                        .visit_declaration_documentation(
                            &declaration.definition,
                            |documentation, _span| {
                                docs |= documentation == "Describes a generic service.";
                                Ok(())
                            },
                        )
                        .is_err()
                    {
                        return false;
                    }
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

/// Proves generic parameters are exposed through HIR with their trait bounds.
#[test]
fn generic_parameters_expose_ordered_bounds_through_hir() -> Result<(), TestFailure> {
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
            let sweep = authority
                .declarations()
                .find(|declaration| {
                    authority
                        .span(&declaration.syntax)
                        .ok()
                        .and_then(|span| authority.source_at(span).ok())
                        .is_some_and(|source| {
                            source
                                .windows(b"fn sweep".len())
                                .any(|window| window == b"fn sweep")
                        })
                })
                .ok_or(RustAuthorityError::MissingSemanticFact {
                    fact: SemanticKind::Function,
                })?;
            let parameters = sweep.definition.generic_params(authority.database);
            if parameters.len() != 1 {
                return Err(RustAuthorityError::MissingSemanticFact {
                    fact: SemanticKind::GenericParameter,
                });
            }
            let parameter = parameters[0];
            if parameter.name(authority.database).as_str() != "T" {
                return Err(RustAuthorityError::MissingSemanticFact {
                    fact: SemanticKind::GenericParameter,
                });
            }
            let ra_ap_hir::GenericParam::TypeParam(type_parameter) = parameter else {
                return Err(RustAuthorityError::MissingSemanticFact {
                    fact: SemanticKind::GenericParameter,
                });
            };
            let bounded = type_parameter
                .trait_bounds(authority.database)
                .into_iter()
                .any(|bound| bound.name(authority.database).as_str() == "Clone");
            if !bounded {
                return Err(RustAuthorityError::MissingSemanticFact {
                    fact: SemanticKind::GenericParameter,
                });
            }
            Ok(())
        })?;
        Ok(())
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

/// Proves RA expands repeated and cfg-selected Rustdoc include attributes after bounded VFS admission.
#[test]
fn rustdoc_include_str_supports_repeated_and_cfg_attributes() -> Result<(), TestFailure> {
    let root = project_root()?;
    let outcome = (|| {
        write_fixture(
            root.join("Cargo.toml"),
            "[package]\nname = \"authority_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[features]\ndoc-extra = []\ndoc-disabled = []\n",
            "write Rustdoc include manifest",
        )?;
        write_fixture(
            root.join("src/lib.rs"),
            "#[doc = include_str!(\"docs.txt\")]\n#[cfg_attr(feature = \"doc-extra\", doc = include_str!(\"docs.txt\"))]\n#[cfg_attr(feature = \"doc-disabled\", doc = include_str!(\"missing-disabled.txt\"))]\n#[doc = \"Repeated literal doc.\"]\npub fn documented() {}\n",
            "write Rustdoc include source",
        )?;
        write_fixture(
            root.join("src/docs.txt"),
            "Included UTF-8 documentation for café.\n",
            "write Rustdoc include contents",
        )?;

        let toolchain = RustToolchain::discover(rustc_path()).map_err(RustAuthorityError::from)?;
        let project = RustProject::open(&root, &toolchain, RustEdition::Rust2024)?;
        let cancelled = AtomicBool::new(false);
        let mut lines = Vec::new();
        let mut expanded_lines = 0_usize;
        project.analyze_with_features(
            RustAnalysisControl {
                cancelled: &cancelled,
                maximum_source_bytes: SourceByteLimit::from(8_192),
                deadline: Instant::now() + std::time::Duration::from_secs(180),
            },
            RustFeatureControl {
                all_features: false,
                no_default_features: true,
                features: &["doc-extra"],
            },
            |authority| {
                let declaration = authority
                    .declarations()
                    .find(|declaration| {
                        matches!(declaration.definition, RustDefinition::Function(_))
                            && authority
                                .declaration_name(declaration)
                                .ok()
                                .and_then(|span| authority.source_at(span).ok())
                                == Some(b"documented")
                    })
                    .ok_or(RustAuthorityError::MissingSemanticFact {
                        fact: SemanticKind::Function,
                    })?;
                authority.visit_declaration_documentation(
                    &declaration.definition,
                    |line, span| {
                        if line.contains("Included UTF-8 documentation") {
                            expanded_lines += 1;
                            if span.is_some() {
                                return Err(RustAuthorityError::MissingSemanticFact {
                                    fact: SemanticKind::Function,
                                });
                            }
                        }
                        lines.push(line.to_owned());
                        Ok(())
                    },
                )?;
                Ok(())
            },
        )?;
        if expanded_lines != 2
            || lines
                .iter()
                .filter(|line| line.as_str() == "Repeated literal doc.")
                .count()
                != 1
        {
            return Err(TestFailure::DocumentationExpansion);
        }
        Ok(())
    })();
    fs::remove_dir_all(&root).map_err(|source| TestFailure::Io {
        operation: "remove Rustdoc include fixture",
        source,
    })?;
    outcome
}

/// Proves local Rustdoc can resolve a bounded include even when workspace discovery ignores it.
#[test]
fn rustdoc_ignored_include_remains_available_locally() -> Result<(), TestFailure> {
    let root = project_root()?;
    let outcome = (|| {
        write_fixture(
            root.join("Cargo.toml"),
            "[package]\nname = \"authority_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
            "write ignored Rustdoc include manifest",
        )?;
        write_fixture(
            root.join(".gitignore"),
            "/src/ignored-doc.txt\n",
            "ignore local Rustdoc include",
        )?;
        write_fixture(
            root.join("src/lib.rs"),
            "#[doc = include_str!(\"ignored-doc.txt\")]\npub fn documented() {}\n",
            "write ignored Rustdoc include source",
        )?;
        write_fixture(
            root.join("src/ignored-doc.txt"),
            "Ignored local documentation remains available.\n",
            "write ignored Rustdoc include contents",
        )?;

        let toolchain = RustToolchain::discover(rustc_path()).map_err(RustAuthorityError::from)?;
        let project = RustProject::open(&root, &toolchain, RustEdition::Rust2024)?;
        let cancelled = AtomicBool::new(false);
        let mut found_local_documentation = false;
        project.analyze(
            RustAnalysisControl {
                cancelled: &cancelled,
                maximum_source_bytes: SourceByteLimit::from(8_192),
                deadline: Instant::now() + std::time::Duration::from_secs(180),
            },
            |authority| {
                let declaration = authority
                    .declarations()
                    .find(|declaration| {
                        matches!(declaration.definition, RustDefinition::Function(_))
                            && authority
                                .declaration_name(declaration)
                                .ok()
                                .and_then(|span| authority.source_at(span).ok())
                                == Some(b"documented")
                    })
                    .ok_or(RustAuthorityError::MissingSemanticFact {
                        fact: SemanticKind::Function,
                    })?;
                authority.visit_declaration_documentation(
                    &declaration.definition,
                    |line, span| {
                        if line.contains("Ignored local documentation remains available.") {
                            found_local_documentation = span.is_none();
                        }
                        Ok(())
                    },
                )?;
                Ok(())
            },
        )?;
        if found_local_documentation {
            Ok(())
        } else {
            Err(TestFailure::DocumentationExpansion)
        }
    })();
    fs::remove_dir_all(&root).map_err(|source| TestFailure::Io {
        operation: "remove ignored Rustdoc include fixture",
        source,
    })?;
    outcome
}

/// Proves missing Rustdoc includes fail before the lowerer can silently accept empty docs.
#[test]
fn rustdoc_missing_include_fails_closed() -> Result<(), TestFailure> {
    let root = project_root()?;
    let outcome = (|| {
        write_fixture(
            root.join("src/lib.rs"),
            "#[doc = include_str!(\"missing.txt\")]\npub fn documented() {}\n",
            "write missing Rustdoc include source",
        )?;
        let toolchain = RustToolchain::discover(rustc_path()).map_err(RustAuthorityError::from)?;
        let project = RustProject::open(&root, &toolchain, RustEdition::Rust2024)?;
        let cancelled = AtomicBool::new(false);
        match project.analyze(
            RustAnalysisControl {
                cancelled: &cancelled,
                maximum_source_bytes: SourceByteLimit::from(8_192),
                deadline: Instant::now() + std::time::Duration::from_secs(180),
            },
            |_| Ok(()),
        ) {
            Err(RustAuthorityError::DocumentationInputMissing { path })
                if path.ends_with("src/missing.txt") =>
            {
                Ok(())
            }
            Err(error) => Err(TestFailure::Authority(error)),
            Ok(()) => Err(TestFailure::DocumentationExpansion),
        }
    })();
    fs::remove_dir_all(&root).map_err(|source| TestFailure::Io {
        operation: "remove missing Rustdoc include fixture",
        source,
    })?;
    outcome
}

/// Proves non-UTF-8 Rustdoc include bytes are rejected under include_str! semantics.
#[test]
fn rustdoc_non_utf8_include_fails_closed() -> Result<(), TestFailure> {
    let root = project_root()?;
    let outcome = (|| {
        write_fixture(
            root.join("src/lib.rs"),
            "#[doc = include_str!(\"bytes.txt\")]\npub fn documented() {}\n",
            "write non-UTF-8 Rustdoc include source",
        )?;
        fs::write(root.join("src/bytes.txt"), [0xff, 0xfe]).map_err(|source| TestFailure::Io {
            operation: "write non-UTF-8 Rustdoc include",
            source,
        })?;
        let toolchain = RustToolchain::discover(rustc_path()).map_err(RustAuthorityError::from)?;
        let project = RustProject::open(&root, &toolchain, RustEdition::Rust2024)?;
        let cancelled = AtomicBool::new(false);
        match project.analyze(
            RustAnalysisControl {
                cancelled: &cancelled,
                maximum_source_bytes: SourceByteLimit::from(8_192),
                deadline: Instant::now() + std::time::Duration::from_secs(180),
            },
            |_| Ok(()),
        ) {
            Err(RustAuthorityError::DocumentationInputUtf8 { path })
                if path.ends_with("src/bytes.txt") =>
            {
                Ok(())
            }
            Err(error) => Err(TestFailure::Authority(error)),
            Ok(()) => Err(TestFailure::DocumentationExpansion),
        }
    })();
    fs::remove_dir_all(&root).map_err(|source| TestFailure::Io {
        operation: "remove non-UTF-8 Rustdoc include fixture",
        source,
    })?;
    outcome
}

/// Proves an include larger than the selected source-byte budget is rejected before allocation.
#[test]
fn rustdoc_oversized_include_fails_closed() -> Result<(), TestFailure> {
    let root = project_root()?;
    let outcome = (|| {
        write_fixture(
            root.join("src/lib.rs"),
            "#[doc = include_str!(\"large.txt\")]\npub fn documented() {}\n",
            "write oversized Rustdoc include source",
        )?;
        write_fixture(
            root.join("src/large.txt"),
            &"x".repeat(128),
            "write oversized Rustdoc include",
        )?;
        let toolchain = RustToolchain::discover(rustc_path()).map_err(RustAuthorityError::from)?;
        let project = RustProject::open(&root, &toolchain, RustEdition::Rust2024)?;
        let cancelled = AtomicBool::new(false);
        match project.analyze(
            RustAnalysisControl {
                cancelled: &cancelled,
                maximum_source_bytes: SourceByteLimit::from(96),
                deadline: Instant::now() + std::time::Duration::from_secs(180),
            },
            |_| Ok(()),
        ) {
            Err(RustAuthorityError::DocumentationInputBudget {
                actual, maximum, ..
            }) if actual > u64::from(*maximum) => Ok(()),
            Err(error) => Err(TestFailure::Authority(error)),
            Ok(()) => Err(TestFailure::DocumentationExpansion),
        }
    })();
    fs::remove_dir_all(&root).map_err(|source| TestFailure::Io {
        operation: "remove oversized Rustdoc include fixture",
        source,
    })?;
    outcome
}

/// Proves computed include paths fail closed before RA can silently omit the documentation.
#[test]
fn rustdoc_computed_include_path_fails_closed() -> Result<(), TestFailure> {
    let root = project_root()?;
    let outcome = (|| {
        write_fixture(
            root.join("src/lib.rs"),
            "#[doc = include_str!(concat!(\"docs\", \".txt\"))]\npub fn documented() {}\n",
            "write computed Rustdoc include source",
        )?;
        let toolchain = RustToolchain::discover(rustc_path()).map_err(RustAuthorityError::from)?;
        let project = RustProject::open(&root, &toolchain, RustEdition::Rust2024)?;
        let cancelled = AtomicBool::new(false);
        match project.analyze(
            RustAnalysisControl {
                cancelled: &cancelled,
                maximum_source_bytes: SourceByteLimit::from(8_192),
                deadline: Instant::now() + std::time::Duration::from_secs(180),
            },
            |_| Ok(()),
        ) {
            Err(RustAuthorityError::UnsupportedDocumentationExpression { .. }) => Ok(()),
            Err(error) => Err(TestFailure::Authority(error)),
            Ok(()) => Err(TestFailure::DocumentationExpansion),
        }
    })();
    fs::remove_dir_all(&root).map_err(|source| TestFailure::Io {
        operation: "remove computed Rustdoc include fixture",
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

/// Proves the exact serde_json 1.0.145 build script is admitted as its own Cargo target.
#[test]
fn serde_json_1_0_145_build_script_resolves_as_a_cargo_target() -> Result<(), TestFailure> {
    const BUILD_SCRIPT: &[u8] = include_bytes!("fixtures/serde_json-1.0.145-build-script.txt");
    let root = project_root()?;
    let outcome = (|| {
        write_fixture(
            root.join("Cargo.toml"),
            "[package]\nname = \"serde_json\"\nversion = \"1.0.145\"\nedition = \"2021\"\nbuild = \"build.rs\"\n",
            "write serde_json build-script manifest",
        )?;
        write_fixture(
            root.join("src/lib.rs"),
            "pub fn fixture() {}\n",
            "write serde_json fixture library",
        )?;
        write_fixture(
            root.join("build.rs"),
            std::str::from_utf8(BUILD_SCRIPT).map_err(|_| TestFailure::BuildScriptFixture)?,
            "write serde_json build script",
        )?;
        let toolchain = RustToolchain::discover(rustc_path()).map_err(RustAuthorityError::from)?;
        let build_script = root.join("build.rs");
        let project =
            RustProject::open_with_source(&root, &build_script, &toolchain, RustEdition::Rust2021)?;
        let cancelled = AtomicBool::new(false);
        project.analyze(
            RustAnalysisControl {
                cancelled: &cancelled,
                maximum_source_bytes: SourceByteLimit::from(8_192),
                deadline: Instant::now() + std::time::Duration::from_secs(180),
            },
            |authority| {
                if authority.source_scope != RustSourceScope::CargoTargetRoot {
                    return Err(RustAuthorityError::MissingSemanticFact {
                        fact: SemanticKind::Function,
                    });
                }
                if authority.source != BUILD_SCRIPT {
                    return Err(RustAuthorityError::SourceBinding {
                        expected: BUILD_SCRIPT.len(),
                        observed: authority.source.len(),
                    });
                }
                let resolved_main = authority.declarations().any(|declaration| {
                    if !matches!(declaration.definition, RustDefinition::Function(_)) {
                        return false;
                    }
                    authority
                        .declaration_name(&declaration)
                        .ok()
                        .and_then(|span| authority.source_at(span).ok())
                        .is_some_and(|name| name == b"main")
                });
                if !resolved_main {
                    return Err(RustAuthorityError::MissingSemanticFact {
                        fact: SemanticKind::Function,
                    });
                }
                Ok(())
            },
        )?;
        Ok(())
    })();
    fs::remove_dir_all(&root).map_err(|source| TestFailure::Io {
        operation: "remove fixture",
        source,
    })?;
    outcome
}

/// Regresses the exact serde 1.0.228 build script beneath an unrelated Cargo workspace.
#[test]
fn serde_1_0_228_build_script_resolves_inside_generated_staging_workspace()
-> Result<(), TestFailure> {
    const BUILD_SCRIPT: &[u8] = include_bytes!("fixtures/serde-1.0.228-build-script.txt");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(TestFailure::Clock)?
        .as_nanos();
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(format!("nudox-serde-staging-{nonce}-{sequence}"));
    let package = workspace.join("serde-1.0.228");
    fs::create_dir_all(package.join("src")).map_err(|source| TestFailure::Io {
        operation: "create staged serde fixture",
        source,
    })?;
    let outcome = (|| {
        write_fixture(
            workspace.join("Cargo.toml"),
            "# nudox-registry-workspace-v1\n[workspace]\nmembers = [\"serde-1.0.228\"]\nresolver = \"2\"\n",
            "write generated staging workspace",
        )?;
        write_fixture(
            package.join("Cargo.toml"),
            "[package]\nname = \"serde\"\nversion = \"1.0.228\"\nedition = \"2021\"\nbuild = \"build.rs\"\n",
            "write serde manifest",
        )?;
        write_fixture(
            package.join("src/lib.rs"),
            "pub fn fixture() {}\n",
            "write serde fixture library",
        )?;
        write_fixture(
            package.join("build.rs"),
            std::str::from_utf8(BUILD_SCRIPT).map_err(|_| TestFailure::BuildScriptFixture)?,
            "write serde build script",
        )?;
        let toolchain = RustToolchain::discover(rustc_path()).map_err(RustAuthorityError::from)?;
        let build_script = package.join("build.rs");
        let project = RustProject::open_with_source(
            &package,
            &build_script,
            &toolchain,
            RustEdition::Rust2021,
        )?;
        let cancelled = AtomicBool::new(false);
        project.analyze(
            RustAnalysisControl {
                cancelled: &cancelled,
                maximum_source_bytes: SourceByteLimit::from(8_192),
                deadline: Instant::now() + std::time::Duration::from_secs(180),
            },
            |authority| {
                if authority.source != BUILD_SCRIPT {
                    return Err(RustAuthorityError::SourceBinding {
                        expected: BUILD_SCRIPT.len(),
                        observed: authority.source.len(),
                    });
                }
                let resolved_main = authority.declarations().any(|declaration| {
                    if !matches!(declaration.definition, RustDefinition::Function(_)) {
                        return false;
                    }
                    authority
                        .declaration_name(&declaration)
                        .ok()
                        .and_then(|span| authority.source_at(span).ok())
                        .is_some_and(|name| name == b"main")
                });
                if !resolved_main {
                    return Err(RustAuthorityError::MissingSemanticFact {
                        fact: SemanticKind::Function,
                    });
                }
                Ok(())
            },
        )?;
        Ok(())
    })();
    fs::remove_dir_all(&workspace).map_err(|source| TestFailure::Io {
        operation: "remove staged serde fixture",
        source,
    })?;
    outcome
}

/// Regresses the exact staged Serde source that is only selected behind `cfg(docsrs)`.
#[test]
fn serde_1_0_228_docsrs_source_is_retained_as_detached_scope() -> Result<(), TestFailure> {
    // These bytes are copied from the staged 1.0.228 archive. Their SHA-256
    // identities are 2e01b1191da5bc6be5ca1f208c362bc5b4e5f9fefc0b097433b9aef19a0af926
    // and 157ca402e23c32f11a4f1797c81afb5e9f08df96768012cf3e3199153aafb2dd.
    const SERDE_ROOT: &[u8] = include_bytes!("fixtures/serde-1.0.228-lib.rs");
    const CRATE_ROOT: &[u8] = include_bytes!("fixtures/serde-1.0.228-core-crate_root.rs");
    let root = project_root()?;
    let outcome = (|| {
        write_fixture(
            root.join("Cargo.toml"),
            "[package]\nname = \"serde\"\nversion = \"1.0.228\"\nedition = \"2021\"\n[lib]\npath = \"src/lib.rs\"\n",
            "write staged serde source-selection manifest",
        )?;
        write_fixture(
            root.join("src/lib.rs"),
            std::str::from_utf8(SERDE_ROOT).map_err(|_| TestFailure::SerdeFixture)?,
            "write exact staged serde crate root",
        )?;
        fs::create_dir_all(root.join("src/core")).map_err(|source| TestFailure::Io {
            operation: "create staged serde core source directory",
            source,
        })?;
        write_fixture(
            root.join("src/core/crate_root.rs"),
            std::str::from_utf8(CRATE_ROOT).map_err(|_| TestFailure::SerdeFixture)?,
            "write exact staged serde docsrs source",
        )?;

        let selected = root.join("src/core/crate_root.rs");
        if fs::read(&selected).map_err(|source| TestFailure::Io {
            operation: "verify exact staged serde docsrs source",
            source,
        })? != CRATE_ROOT
        {
            return Err(TestFailure::SerdeFixture);
        }
        let toolchain = RustToolchain::discover(rustc_path()).map_err(RustAuthorityError::from)?;
        let project =
            RustProject::open_with_source(&root, &selected, &toolchain, RustEdition::Rust2021)?;
        let cancelled = AtomicBool::new(false);
        let result = project.analyze(
            RustAnalysisControl {
                cancelled: &cancelled,
                maximum_source_bytes: SourceByteLimit::from(16_384),
                deadline: Instant::now() + std::time::Duration::from_secs(180),
            },
            |_authority| Ok(()),
        );
        let selected_canonical = selected.canonicalize().map_err(|source| TestFailure::Io {
            operation: "canonicalize staged serde docsrs source",
            source,
        })?;
        match result {
            Err(RustAuthorityError::DetachedSource {
                path,
                active_hir_roots,
            }) if path == selected_canonical => {
                assert!(active_hir_roots.package_crate_count > 0);
                assert_eq!(
                    active_hir_roots.package_crate_count,
                    active_hir_roots.package_relative_roots.len()
                        + active_hir_roots.omitted_package_crates
                );
                assert!(active_hir_roots.package_relative_roots.len() <= 16);
                assert!(
                    active_hir_roots
                        .package_relative_roots
                        .iter()
                        .any(|root| root == Path::new("src/lib.rs"))
                );
                assert!(
                    !active_hir_roots
                        .package_relative_roots
                        .iter()
                        .any(|root| root == Path::new("src/core/crate_root.rs"))
                );
                Ok(())
            }
            _ => Err(TestFailure::SerdeSourceScope),
        }
    })();
    fs::remove_dir_all(&root).map_err(|source| TestFailure::Io {
        operation: "remove staged serde source fixture",
        source,
    })?;
    outcome
}

/// Proves a separately selected ordinary module is owned by its active Cargo target.
#[test]
fn separately_selected_active_module_keeps_its_cargo_scope() -> Result<(), TestFailure> {
    const MODULE: &[u8] = b"pub fn selected() {}\n";
    let root = project_root()?;
    let outcome = (|| {
        write_fixture(
            root.join("Cargo.toml"),
            "[package]\nname = \"authority_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            "write active-module manifest",
        )?;
        write_fixture(
            root.join("src/lib.rs"),
            "mod child;\n",
            "write active-module crate root",
        )?;
        write_fixture(
            root.join("src/child.rs"),
            std::str::from_utf8(MODULE).map_err(|_| TestFailure::SerdeFixture)?,
            "write active selected module",
        )?;
        let selected = root.join("src/child.rs");
        let toolchain = RustToolchain::discover(rustc_path()).map_err(RustAuthorityError::from)?;
        let project =
            RustProject::open_with_source(&root, &selected, &toolchain, RustEdition::Rust2021)?;
        let cancelled = AtomicBool::new(false);
        project.analyze(
            RustAnalysisControl {
                cancelled: &cancelled,
                maximum_source_bytes: SourceByteLimit::from(1_024),
                deadline: Instant::now() + std::time::Duration::from_secs(180),
            },
            |authority| {
                let selected_function = authority.declarations().any(|declaration| {
                    matches!(declaration.definition, RustDefinition::Function(_))
                        && authority
                            .declaration_name(&declaration)
                            .ok()
                            .and_then(|span| authority.source_at(span).ok())
                            == Some(b"selected")
                });
                if authority.source != MODULE
                    || authority.source_scope != RustSourceScope::CargoModule
                    || !selected_function
                {
                    return Err(RustAuthorityError::MissingSemanticFact {
                        fact: SemanticKind::Function,
                    });
                }
                Ok(())
            },
        )?;
        Ok(())
    })();
    fs::remove_dir_all(&root).map_err(|source| TestFailure::Io {
        operation: "remove active-module fixture",
        source,
    })?;
    outcome
}

/// Proves package files share the exact loaded database and resolve across that session.
#[test]
fn package_workspace_reuses_database_for_root_and_sibling_sources() -> Result<(), TestFailure> {
    let root = project_root()?;
    let outcome = (|| {
        let root_source = fs::read(root.join("src/lib.rs")).map_err(|source| TestFailure::Io {
            operation: "read workspace root source",
            source,
        })?;
        let sibling_path = root.join("src/sibling.rs");
        let sibling_source = fs::read(&sibling_path).map_err(|source| TestFailure::Io {
            operation: "read workspace sibling source",
            source,
        })?;
        let toolchain = RustToolchain::discover(rustc_path()).map_err(RustAuthorityError::from)?;
        let cancelled = AtomicBool::new(false);
        let control = RustAnalysisControl {
            cancelled: &cancelled,
            maximum_source_bytes: SourceByteLimit::from(8_192),
            deadline: Instant::now() + std::time::Duration::from_secs(180),
        };
        let workspace = RustWorkspace::open(&root, &toolchain, RustEdition::Rust2024, control)?;
        let (root_database, root_resolves_sibling) = workspace.analyze_source(
            root.join("src/lib.rs"),
            &root_source,
            control,
            |authority| {
                let resolves_sibling = authority.paths().any(|path| {
                    authority
                        .span(path.syntax())
                        .ok()
                        .and_then(|span| authority.source_at(span).ok())
                        .is_some_and(|spelling| spelling == b"sibling::assist")
                        && authority.resolve_path(&path).is_some()
                });
                Ok((authority.database as *const _ as usize, resolves_sibling))
            },
        )?;
        let sibling_database =
            workspace.analyze_source(&sibling_path, &sibling_source, control, |authority| {
                Ok((
                    authority.database as *const _ as usize,
                    authority.source_scope,
                ))
            })?;
        if root_database != sibling_database.0
            || !root_resolves_sibling
            || sibling_database.1 != RustSourceScope::CargoModule
        {
            return Err(TestFailure::WorkspaceSession);
        }

        // A same-length mutation must fail before the callback can observe HIR.
        let mut mismatched = root_source.clone();
        let changed = mismatched
            .iter_mut()
            .find(|byte| **byte == b'e')
            .ok_or(TestFailure::WorkspaceSession)?;
        *changed = b'x';
        let entered = std::cell::Cell::new(false);
        let mismatch = workspace.analyze_source(
            root.join("src/lib.rs"),
            &mismatched,
            control,
            |_authority| {
                entered.set(true);
                Ok(())
            },
        );
        if entered.get()
            || !matches!(
                mismatch,
                Err(RustAuthorityError::SourceBinding { expected, observed })
                    if expected == mismatched.len() && observed == root_source.len()
            )
        {
            return Err(TestFailure::WorkspaceSourceBinding);
        }
        Ok(())
    })();
    fs::remove_dir_all(&root).map_err(|source| TestFailure::Io {
        operation: "remove workspace-session fixture",
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
    /// Rustdoc include expansion or its failure boundary did not match rust-analyzer semantics.
    #[error("Rustdoc include expansion did not preserve the expected bounded documentation")]
    DocumentationExpansion,
    /// The exact registry build-script fixture could not be converted back into source text.
    #[error("serde_json build-script regression fixture was not valid UTF-8")]
    BuildScriptFixture,
    /// The retained staged Serde sources were not exact UTF-8 fixture bytes.
    #[error("staged Serde fixture bytes were not exact UTF-8 source bytes")]
    SerdeFixture,
    /// The exact docsrs-gated Serde source was not classified as detached.
    #[error("the exact docsrs-gated Serde source was not classified as detached")]
    SerdeSourceScope,
    /// Package sources did not share one analyzer database or resolve across files.
    #[error("package Rust sources did not share the expected Cargo workspace session")]
    WorkspaceSession,
    /// Source bytes that differed from the VFS were not rejected before HIR entry.
    #[error("package Rust source identity mismatch entered the analyzer callback")]
    WorkspaceSourceBinding,
    /// Rust authority returned a typed failure.
    #[error("Rust authority failed: {0}")]
    Authority(#[from] RustAuthorityError),
}
