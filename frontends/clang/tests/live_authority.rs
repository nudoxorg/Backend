//! Proves direct C and C++ libclang collection when a test environment provisions libclang.
//! The tests require the `native-test` feature and fail if the configured authority is unavailable.
//! They exercise profiles, macros, includes, overload identities, recursive types, docs, and references.

#![cfg(feature = "native-test")]

use backend_frontend_clang::legacy::{
    BuiltinClass, ClangInput, ClangScratch, CollectError, DeclarationFact, DeclarationId,
    DeclarationKind, DefinitionState, DiagnosticFact, IncludeFact, MAX_CLANG_DECLARATIONS,
    MAX_CLANG_DIAGNOSTICS, MAX_CLANG_INCLUDES, MAX_CLANG_OVERRIDES, MAX_CLANG_REFERENCES,
    MAX_CLANG_TYPE_EDGES, MAX_CLANG_TYPES, MethodVirtuality, OverrideFact, ReferenceFact,
    ReferenceKind, ReferenceTarget, SourceDependencyKind, SourceSpan, StorageClass, SymbolIdentity,
    TypeEdge, TypeFact, TypeId, collect,
    facts::{TypeKind, TypeQualifiers},
};
use backend_semantic::vocabulary::{CStandard, CxxStandard};
use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

/// Identifies the one direct native fact a live authority proof requires.
#[derive(Clone, Copy, Debug, thiserror::Error)]
enum RequiredFact {
    /// A declaration kind was not emitted.
    #[error("declaration {kind:?}")]
    Declaration {
        /// Required direct native declaration kind.
        kind: DeclarationKind,
    },
    /// A recursive type kind was not emitted.
    #[error("type {kind:?}")]
    Type {
        /// Required direct native recursive type kind.
        kind: TypeKind,
    },
    /// A recursive type relation was not emitted.
    #[error("type relation {relation:?}")]
    TypeRelation {
        /// Required direct native recursive type relation.
        relation: backend_frontend_clang::legacy::TypeRelation,
    },
    /// A source dependency kind was not emitted.
    #[error("source dependency {kind:?}")]
    Dependency {
        /// Required direct native source dependency kind.
        kind: SourceDependencyKind,
    },
    /// A local call target was not resolved by libclang.
    #[error("local call")]
    LocalCall,
    /// A reference site did not slice to the exact required written name.
    #[error("reference site not slicing to {required:?}")]
    ReferenceName {
        /// The exact identifier bytes the site must carry.
        required: &'static [u8],
    },
    /// A declaration documentation span was not emitted.
    #[error("documentation")]
    Documentation,
    /// Distinct overload declarations did not retain distinct native identities.
    #[error("distinct overload identity")]
    OverloadIdentity,
}

/// Holds exact native collection failures and missing fact claims for the live authority tests.
#[derive(Debug, thiserror::Error)]
enum TestError {
    /// Direct libclang collection returned its exact typed error.
    #[error(transparent)]
    Collection(#[from] CollectError),
    /// The real compilation database could not be loaded.
    #[error(transparent)]
    Database(#[from] backend_frontend_clang::legacy::DatabaseError),
    /// A required direct native fact was absent from the returned bounded fact prefixes.
    #[error("missing direct native fact: {0}")]
    Missing(RequiredFact),
}

#[test]
fn c_authority_retains_macro_include_docs_recursive_types_and_local_calls() -> Result<(), TestError>
{
    let source = br#"
#include "authority_missing_header.h"
#define SCALE(value) ((value) * 2)
/// Adds two values.
int add(int left, int right) { return left + right; }
int caller(int *value) { return SCALE(add(*value, 2)); }
"#;
    with_scratch(|scratch| {
        let facts = collect(
            ClangInput::C {
                file_name: c"authority.c",
                source,
                standard: CStandard::C23,
            },
            scratch,
        )?;
        require_declaration(&facts, DeclarationKind::Macro)?;
        require_declaration(&facts, DeclarationKind::Function)?;
        require_dependency(&facts, SourceDependencyKind::Include)?;
        require_type(&facts, TypeKind::Pointer)?;
        require_local_call(&facts)?;
        if facts
            .declarations
            .iter()
            .any(|declaration| declaration.documentation.is_some())
        {
            Ok(())
        } else {
            Err(TestError::Missing(RequiredFact::Documentation))
        }
    })
}

/// Reference sites carry the written name token of the referenced entity,
/// never the whole expression extent.
///
/// The regression this pins: a call site once carried the entire
/// `measure(buffer)` extent and a member read the entire `buffer->content`
/// extent, so every downstream occurrence site read as full expressions.
/// Each emitted span is proven against the exact source bytes, and every
/// local-target site's bytes equal its target declaration's own name bytes.
#[test]
fn reference_spans_slice_to_the_referenced_name_tokens() -> Result<(), TestError> {
    let source = br#"
struct Buffer { int content; };
static int total;
int measure(struct Buffer *buffer) {
    total = total + buffer->content;
    return measure(buffer) + total;
}
"#;
    with_scratch(|scratch| {
        let facts = collect(
            ClangInput::C {
                file_name: c"reference-names.c",
                source,
                standard: CStandard::C23,
            },
            scratch,
        )?;
        assert!(!facts.references.is_empty());
        for reference in facts.references {
            println!(
                "probe kind={:?} span={:?} bytes={:?} target={:?}",
                reference.kind,
                reference.span,
                String::from_utf8_lossy(source_at(source, reference.span)),
                match reference.target {
                    ReferenceTarget::Local(identity) => identity
                        .bytes
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>(),
                    ReferenceTarget::Foreign { identity, .. } => identity
                        .bytes
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>(),
                    ReferenceTarget::Unresolved => "unresolved".to_owned(),
                },
            );
        }
        // Every reference site slices to exactly one source identifier.
        for reference in facts.references {
            let bytes = source_at(source, reference.span);
            assert!(
                !bytes.is_empty()
                    && !bytes[0].is_ascii_digit()
                    && bytes
                        .iter()
                        .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_'),
                "reference at {:?} must slice to one identifier, got {bytes:?}",
                reference.span,
            );
        }
        // A member read slices to the member name, never `buffer->content`.
        let member = facts
            .references
            .iter()
            .find(|reference| {
                reference.kind == ReferenceKind::Member
                    && source_at(source, reference.span) == b"content"
            })
            .ok_or(TestError::Missing(RequiredFact::ReferenceName {
                required: b"content",
            }))?;
        // A call site slices to the callee name, never `measure(buffer)`.
        facts
            .references
            .iter()
            .find(|reference| {
                reference.kind == ReferenceKind::Call
                    && source_at(source, reference.span) == b"measure"
            })
            .ok_or(TestError::Missing(RequiredFact::ReferenceName {
                required: b"measure",
            }))?;
        // A plain declaration reference slices to the variable name.
        facts
            .references
            .iter()
            .find(|reference| {
                reference.kind == ReferenceKind::Value
                    && source_at(source, reference.span) == b"total"
            })
            .ok_or(TestError::Missing(RequiredFact::ReferenceName {
                required: b"total",
            }))?;
        // The narrowing keeps the site inside the written expression: the
        // member token is strictly narrower than the whole extent gate.
        let expression_start = source
            .windows(15)
            .position(|window| window == b"buffer->content")
            .expect("member expression present");
        assert!(
            usize::try_from(member.span.start)
                .is_ok_and(|start| start > expression_start),
            "member site must start at the member token, not the expression"
        );
        // Every local-target site's bytes equal its target's declared name bytes.
        for reference in facts.references {
            let ReferenceTarget::Local(identity) = reference.target else {
                continue;
            };
            let Some(declaration) = facts
                .declarations
                .iter()
                .find(|declaration| declaration.identity == Some(identity))
            else {
                continue;
            };
            let Some(name) = declaration.name else {
                continue;
            };
            assert_eq!(
                source_at(source, reference.span),
                source_at(source, name),
                "site at {:?} must read exactly its target's declared name",
                reference.span,
            );
        }
        Ok(())
    })
}

/// A flag-first database argument vector must survive intact.
///
/// This is the exact shape `ClangProject::arguments` supplies to the engine's
/// project lane: system include arguments first, per-extension defaults last,
/// no compiler executable. The former unconditional argv[0] strip ate the
/// leading `-isystem` flag, so the vendor directory became a stray positional
/// input and `<pkg/vendored.hpp>` lost its only search path.
#[test]
fn flag_first_database_arguments_keep_the_first_include_flag() -> Result<(), TestError> {
    let root = std::env::temp_dir().join("nudox-clang-flag-first-args");
    let vendor = root.join("vendor/include/pkg");
    fs::create_dir_all(&vendor).map_err(|_| {
        TestError::Missing(RequiredFact::Dependency {
            kind: SourceDependencyKind::Include,
        })
    })?;
    let source = b"#include <pkg/vendored.hpp>\nint main(void) { return VENDORED; }\n";
    fs::write(vendor.join("vendored.hpp"), b"#define VENDORED 42\n").map_err(|_| {
        TestError::Missing(RequiredFact::Dependency {
            kind: SourceDependencyKind::Include,
        })
    })?;
    let source_path = root.join("src/main.cpp");
    fs::create_dir_all(source_path.parent().expect("parent exists")).map_err(|_| {
        TestError::Missing(RequiredFact::Dependency {
            kind: SourceDependencyKind::Include,
        })
    })?;
    fs::write(&source_path, source).map_err(|_| {
        TestError::Missing(RequiredFact::Declaration {
            kind: DeclarationKind::Function,
        })
    })?;
    let arguments = vec![
        "-isystem".to_owned(),
        root.join("vendor/include").to_string_lossy().into_owned(),
        "-std=c++17".to_owned(),
        "-x".to_owned(),
        "c++".to_owned(),
    ];
    let owned = arguments
        .iter()
        .map(|argument| std::ffi::CString::new(argument.as_bytes()).expect("no interior NUL"))
        .collect::<Vec<_>>();
    let borrowed = owned
        .iter()
        .map(std::ffi::CString::as_c_str)
        .collect::<Vec<_>>();
    let result = with_scratch(|scratch| {
        let file_name = std::ffi::CString::new(source_path.to_string_lossy().as_bytes())
            .expect("no interior NUL");
        let directory =
            std::ffi::CString::new(root.to_string_lossy().as_bytes()).expect("no interior NUL");
        let input =
            ClangInput::from_database(&file_name, source, &borrowed, &directory).map_err(|_| {
                TestError::Missing(RequiredFact::Dependency {
                    kind: SourceDependencyKind::Include,
                })
            })?;
        let facts = collect(input, scratch)?;
        if facts
            .includes
            .iter()
            .any(|dependency| dependency.resolved.is_some())
        {
            require_declaration(&facts, DeclarationKind::Function)
        } else {
            Err(TestError::Missing(RequiredFact::Dependency {
                kind: SourceDependencyKind::Include,
            }))
        }
    });
    let cleanup = fs::remove_dir_all(&root);
    result.and(cleanup.map_err(|_| {
        TestError::Missing(RequiredFact::Declaration {
            kind: DeclarationKind::Function,
        })
    }))
}

/// A `.hpp` single-header entry parses end to end through
/// `ClangProject::arguments`, the exact call shape the engine's project lane
/// uses. Both corpus regressions are anchored here: the C++ header defaults
/// (`-x c++`, never `-std=c11`) and the intact flag-first argument vector.
#[test]
fn hpp_single_header_entry_parses_through_project_arguments() -> Result<(), TestError> {
    let dir = tempfile::tempdir().map_err(|_| {
        TestError::Missing(RequiredFact::Declaration {
            kind: DeclarationKind::Namespace,
        })
    })?;
    let root = dir.path().join("single_include/demo");
    fs::create_dir_all(&root).map_err(|_| {
        TestError::Missing(RequiredFact::Declaration {
            kind: DeclarationKind::Namespace,
        })
    })?;
    let header = root.join("demo.hpp");
    let source = b"namespace demo {\nstruct Widget { int value; };\n}\n";
    fs::write(&header, source).map_err(|_| {
        TestError::Missing(RequiredFact::Declaration {
            kind: DeclarationKind::Namespace,
        })
    })?;
    let project =
        backend_frontend_clang::ClangProject::open(dir.path(), &header).map_err(|_| {
            TestError::Missing(RequiredFact::Declaration {
                kind: DeclarationKind::Namespace,
            })
        })?;
    let arguments = project.arguments();
    let owned = arguments
        .iter()
        .map(|argument| std::ffi::CString::new(argument.as_bytes()).expect("no interior NUL"))
        .collect::<Vec<_>>();
    let borrowed = owned
        .iter()
        .map(std::ffi::CString::as_c_str)
        .collect::<Vec<_>>();
    with_scratch(|scratch| {
        let file_name = std::ffi::CString::new(project.entry().to_string_lossy().as_bytes())
            .expect("no interior NUL");
        let directory = std::ffi::CString::new(project.root().to_string_lossy().as_bytes())
            .expect("no interior NUL");
        let input =
            ClangInput::from_database(&file_name, source, &borrowed, &directory).map_err(|_| {
                TestError::Missing(RequiredFact::Declaration {
                    kind: DeclarationKind::Namespace,
                })
            })?;
        let facts = collect(input, scratch)?;
        let namespace = facts
            .declarations
            .iter()
            .find(|declaration| declaration.kind == DeclarationKind::Namespace)
            .ok_or(TestError::Missing(RequiredFact::Declaration {
                kind: DeclarationKind::Namespace,
            }))?;
        assert_eq!(
            namespace.name.map(|span| source_at(source, span)),
            Some(&b"demo"[..])
        );
        let record = facts
            .declarations
            .iter()
            .find(|declaration| declaration.kind == DeclarationKind::Record)
            .ok_or(TestError::Missing(RequiredFact::Declaration {
                kind: DeclarationKind::Record,
            }))?;
        assert_eq!(
            record.name.map(|span| source_at(source, span)),
            Some(&b"Widget"[..])
        );
        Ok(())
    })
}

/// A concrete member pointer admits both operands: the owning class edge and
/// the pointee edge, straight from the direct libclang queries.
#[test]
fn cxx_authority_concrete_member_pointer_keeps_owner_and_pointee_edges() -> Result<(), TestError> {
    let source = br"
struct Widget { void run(); };
void (Widget::*slot)() = &Widget::run;
";
    with_scratch(|scratch| {
        let facts = collect(
            ClangInput::Cxx {
                file_name: c"member-pointer.cc",
                source,
                standard: CxxStandard::Cxx23,
            },
            scratch,
        )?;
        let member_pointer = facts
            .types
            .iter()
            .find(|type_fact| type_fact.kind == TypeKind::MemberPointer)
            .ok_or(TestError::Missing(RequiredFact::Type {
                kind: TypeKind::MemberPointer,
            }))?;
        assert!(
            facts.type_edges.iter().any(|edge| {
                edge.source == member_pointer.id
                    && edge.relation == backend_frontend_clang::legacy::TypeRelation::MemberOwner
            }),
            "a concrete member pointer must retain its owning-class edge"
        );
        assert!(
            facts.type_edges.iter().any(|edge| {
                edge.source == member_pointer.id
                    && edge.relation == backend_frontend_clang::legacy::TypeRelation::Pointee
            }),
            "a member pointer must retain its pointee edge"
        );
        Ok(())
    })
}

/// A dependent member pointer (`T::*` under a template parameter) must not
/// crash the native authority and must keep the member-pointer fact with its
/// pointee while omitting the class edge.
///
/// This is the catch2 corpus regression: the first member pointer visited in
/// `catch2/single_include/catch2/catch.hpp` is the dependent
/// `void (C::*)()`, and the loaded libclang crashes inside
/// `clang_Type_getClassType` on exactly that form (the dependent class
/// operand is stored as a nested-name-specifier, not a type), which the
/// corpus audit observed as an immediate SIGSEGV row crash.
#[test]
fn cxx_authority_dependent_member_pointer_survives_without_owner_edge() -> Result<(), TestError> {
    let source = br"
template <typename C> struct Probe { void (C::*slot)(); };
";
    with_scratch(|scratch| {
        let facts = collect(
            ClangInput::Cxx {
                file_name: c"dependent-member-pointer.cc",
                source,
                standard: CxxStandard::Cxx23,
            },
            scratch,
        )?;
        let member_pointer = facts
            .types
            .iter()
            .find(|type_fact| type_fact.kind == TypeKind::MemberPointer)
            .ok_or(TestError::Missing(RequiredFact::Type {
                kind: TypeKind::MemberPointer,
            }))?;
        assert!(
            !facts
                .type_edges
                .iter()
                .any(|edge| edge.relation
                    == backend_frontend_clang::legacy::TypeRelation::MemberOwner),
            "a dependent member pointer has no concrete owning-class operand"
        );
        assert!(
            facts.type_edges.iter().any(|edge| {
                edge.source == member_pointer.id
                    && edge.relation == backend_frontend_clang::legacy::TypeRelation::Pointee
            }),
            "a dependent member pointer keeps its pointee edge"
        );
        Ok(())
    })
}

/// Runs the exact largest-source entry of each real corpus package through
/// the project lane and asserts a successful authority.
///
/// Guarded: skips when `NUDOX_CLANG_CORPUS_DIR` is unset or a package root is
/// absent. The entry selection mirrors the flow resolver: largest source file
/// by bytes among the C-family extensions.
#[test]
fn real_corpus_selected_entries_reach_a_successful_authority() -> Result<(), TestError> {
    let Ok(corpus) = std::env::var("NUDOX_CLANG_CORPUS_DIR") else {
        return Ok(());
    };
    for package in ["nlohmann-json", "catch2"] {
        let root = std::path::PathBuf::from(&corpus).join(package);
        if !root.is_dir() {
            continue;
        }
        let mut files: Vec<(std::path::PathBuf, u64)> = Vec::new();
        collect_sources(&root, &mut files)?;
        files.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        let Some((entry, bytes)) = files.first().cloned() else {
            continue;
        };
        let source = fs::read(&entry).map_err(|_| {
            TestError::Missing(RequiredFact::Declaration {
                kind: DeclarationKind::Namespace,
            })
        })?;
        assert_eq!(source.len() as u64, bytes, "selected entry bytes");
        let project = backend_frontend_clang::ClangProject::open(&root, &entry).map_err(|_| {
            TestError::Missing(RequiredFact::Declaration {
                kind: DeclarationKind::Namespace,
            })
        })?;
        let arguments = project.arguments();
        assert!(
            arguments.ends_with(&["-std=c++17".to_owned(), "-x".to_owned(), "c++".to_owned()]),
            "{package} selects a C++ header entry and must parse as C++: {arguments:?}"
        );
        let owned = arguments
            .iter()
            .map(|argument| std::ffi::CString::new(argument.as_bytes()).expect("no interior NUL"))
            .collect::<Vec<_>>();
        let file_name = std::ffi::CString::new(project.entry().to_string_lossy().as_bytes())
            .expect("no interior NUL");
        let directory = std::ffi::CString::new(project.root().to_string_lossy().as_bytes())
            .expect("no interior NUL");
        let collected = std::thread::Builder::new()
            .name(format!("real-corpus-{package}"))
            .stack_size(512 * 1024 * 1024)
            .spawn(move || {
                let borrowed = owned
                    .iter()
                    .map(std::ffi::CString::as_c_str)
                    .collect::<Vec<_>>();
                let mut declarations = vec![empty_declaration(); MAX_CLANG_DECLARATIONS];
                let mut types = vec![empty_type(); MAX_CLANG_TYPES];
                let mut type_edges = vec![empty_type_edge(); MAX_CLANG_TYPE_EDGES];
                let mut references = vec![empty_reference(); MAX_CLANG_REFERENCES];
                let mut diagnostics = vec![empty_diagnostic(); MAX_CLANG_DIAGNOSTICS];
                let mut includes = vec![empty_include(); MAX_CLANG_INCLUDES];
                let mut overrides = vec![empty_override(); MAX_CLANG_OVERRIDES];
                let input = ClangInput::from_database(&file_name, &source, &borrowed, &directory)
                    .expect("corpus entry admits a database input");
                collect(
                    input,
                    ClangScratch {
                        declarations: &mut declarations,
                        types: &mut types,
                        type_edges: &mut type_edges,
                        references: &mut references,
                        diagnostics: &mut diagnostics,
                        includes: &mut includes,
                        overrides: &mut overrides,
                    },
                )
                .map(|facts| {
                    (
                        facts.declarations.len(),
                        facts.types.len(),
                        facts.references.len(),
                        facts.includes.len(),
                    )
                })
            })
            .expect("corpus worker thread")
            .join()
            .expect("corpus worker finished without a native crash");
        let (declarations, types, references, includes) =
            collected.map_err(|error| TestError::Collection(error))?;
        println!(
            "real-corpus-authority package={package} entry={} bytes={bytes} declarations={declarations} types={types} references={references} includes={includes}",
            entry.display(),
        );
        assert!(declarations > 0, "{package} entry must yield declarations");
    }
    Ok(())
}

#[test]
fn database_authority_parses_relative_translation_unit_with_real_include() -> Result<(), TestError>
{
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| {
            TestError::Missing(RequiredFact::Declaration {
                kind: DeclarationKind::Function,
            })
        })?
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nudox-clang-authority-{suffix}"));
    let source_path = root.join("src/main.c");
    fs::create_dir_all(root.join("src")).map_err(|_| {
        TestError::Missing(RequiredFact::Dependency {
            kind: SourceDependencyKind::Include,
        })
    })?;
    fs::create_dir_all(root.join("include")).map_err(|_| {
        TestError::Missing(RequiredFact::Dependency {
            kind: SourceDependencyKind::Include,
        })
    })?;
    let source = b"#include \"base.h\"\nint main(void) { return BASE; }\n";
    fs::write(root.join("include/base.h"), b"#define BASE 7\n").map_err(|_| {
        TestError::Missing(RequiredFact::Dependency {
            kind: SourceDependencyKind::Include,
        })
    })?;
    fs::write(&source_path, source).map_err(|_| {
        TestError::Missing(RequiredFact::Declaration {
            kind: DeclarationKind::Function,
        })
    })?;
    let database = format!(
        "[{{\"directory\":\"{}\",\"file\":\"src/main.c\",\"arguments\":[\"clang\",\"-x\",\"c\",\"-std=c23\",\"-I\",\"include\",\"src/main.c\"]}}]",
        root.display()
    );
    fs::write(root.join("compile_commands.json"), database).map_err(|_| {
        TestError::Missing(RequiredFact::Declaration {
            kind: DeclarationKind::Function,
        })
    })?;
    let result = (|| {
        let database = backend_frontend_clang::legacy::CompilationDatabase::from_directory(&root)?;
        let command =
            database
                .commands()
                .first()
                .ok_or(TestError::Missing(RequiredFact::Declaration {
                    kind: DeclarationKind::Function,
                }))?;
        let arguments = command
            .arguments()
            .iter()
            .map(|argument| argument.as_c_str())
            .collect::<Vec<_>>();
        let input =
            ClangInput::from_database(command.file_name(), source, &arguments, command.directory())
                .map_err(|_| {
                    TestError::Missing(RequiredFact::Declaration {
                        kind: DeclarationKind::Function,
                    })
                })?;
        with_scratch(|scratch| {
            let facts = collect(input, scratch)?;
            require_declaration(&facts, DeclarationKind::Function)?;
            require_dependency(&facts, SourceDependencyKind::Include)
        })
    })();
    let cleanup = fs::remove_dir_all(&root);
    result.and(cleanup.map_err(|_| {
        TestError::Missing(RequiredFact::Declaration {
            kind: DeclarationKind::Function,
        })
    }))
}

#[test]
fn cxx_authority_keeps_overload_identity_and_template_type_edges() -> Result<(), TestError> {
    let source = br"
template <typename Item> struct Box { Item value; };
int score(int value) { return value; }
double score(double value) { return value; }
int caller() { Box<int> value{2}; return score(value.value); }
";
    with_scratch(|scratch| {
        let facts = collect(
            ClangInput::Cxx {
                file_name: c"authority.cc",
                source,
                standard: CxxStandard::Cxx23,
            },
            scratch,
        )?;
        require_declaration(&facts, DeclarationKind::Template)?;
        require_declaration(&facts, DeclarationKind::Function)?;
        require_type(&facts, TypeKind::Function)?;
        require_type_relation(
            &facts,
            backend_frontend_clang::legacy::TypeRelation::TemplateArgument,
        )?;
        require_local_call(&facts)?;
        distinct_score_overload_identity(&facts, source)
    })
}

#[test]
fn cxx_authority_retains_virtuality_and_deduplicated_overrides() -> Result<(), TestError> {
    let source = br#"
struct Base { virtual void run() = 0; };
struct Derived : Base { void run() override; };
void Derived::run() {}
"#;
    with_scratch(|scratch| {
        let facts = collect(
            ClangInput::Cxx {
                file_name: c"authority.cc",
                source,
                standard: CxxStandard::Cxx23,
            },
            scratch,
        )?;
        let base = method_identity(
            &facts,
            source,
            b"run",
            b"Base",
            MethodVirtuality::PureVirtual,
        )?;
        let derived = method_identity(
            &facts,
            source,
            b"run",
            b"Derived",
            MethodVirtuality::Virtual,
        )?;
        assert_eq!(facts.overrides.len(), 1);
        assert_eq!(
            facts.overrides[0],
            OverrideFact {
                source: derived,
                target: base,
                target_file: None,
            }
        );

        let mut declarations = [empty_declaration(); DECLARATION_SLOTS];
        let mut types = [empty_type(); TYPE_SLOTS];
        let mut type_edges = [empty_type_edge(); TYPE_EDGE_SLOTS];
        let mut references = [empty_reference(); REFERENCE_SLOTS];
        let mut diagnostics = [empty_diagnostic(); DIAGNOSTIC_SLOTS];
        let mut includes = [empty_include(); DEPENDENCY_SLOTS];
        let mut overrides = [];
        let result = collect(
            ClangInput::Cxx {
                file_name: c"authority.cc",
                source,
                standard: CxxStandard::Cxx23,
            },
            ClangScratch {
                declarations: &mut declarations,
                types: &mut types,
                type_edges: &mut type_edges,
                references: &mut references,
                diagnostics: &mut diagnostics,
                includes: &mut includes,
                overrides: &mut overrides,
            },
        );
        match result {
            Err(CollectError::ScratchCapacity {
                lane: backend_frontend_clang::legacy::ScratchLane::Overrides,
                capacity: 0,
                required: 1,
            }) => Ok(()),
            Err(error) => Err(TestError::Collection(error)),
            Ok(_) => Err(TestError::Missing(RequiredFact::OverloadIdentity)),
        }
    })
}

#[test]
fn c_authority_anonymous_record_keeps_only_the_unnamed_definition() -> Result<(), TestError> {
    let source = br"struct { int x; } point;";
    with_scratch(|scratch| {
        let facts = collect(
            ClangInput::C {
                file_name: c"anonymous.c",
                source,
                standard: CStandard::C23,
            },
            scratch,
        )?;
        let records = facts
            .declarations
            .iter()
            .filter(|fact| fact.kind == DeclarationKind::Record)
            .count();
        assert_eq!(records, 1);
        let record = facts
            .declarations
            .iter()
            .find(|fact| fact.kind == DeclarationKind::Record)
            .ok_or(TestError::Missing(RequiredFact::Declaration {
                kind: DeclarationKind::Record,
            }))?;
        assert_eq!(record.name, None);
        assert_eq!(record.definition, DefinitionState::Definition);
        assert!(facts.declarations.iter().any(|fact| {
            fact.kind == DeclarationKind::Field
                && fact
                    .name
                    .is_some_and(|span| source_at(source, span) == b"x")
                && fact.owner == record.identity
        }));
        assert!(facts.declarations.iter().any(|fact| {
            fact.kind == DeclarationKind::Variable
                && fact
                    .name
                    .is_some_and(|span| source_at(source, span) == b"point")
        }));
        assert!(facts.declarations.iter().all(|fact| {
            fact.name
                .is_none_or(|span| source_at(source, span) != b"struct")
        }));
        Ok(())
    })
}

#[test]
fn c_authority_deduplicates_canonical_anonymous_record_cursors() -> Result<(), TestError> {
    let source = br"struct { int x; } point;";
    with_scratch(|scratch| {
        let facts = collect(
            ClangInput::C {
                file_name: c"duplicate-anonymous.c",
                source,
                standard: CStandard::C23,
            },
            scratch,
        )?;
        assert_eq!(
            facts
                .declarations
                .iter()
                .filter(|fact| fact.kind == DeclarationKind::Record)
                .count(),
            1
        );
        Ok(())
    })
}

#[test]
fn cxx_authority_canonical_record_dedupe_prefers_definition() -> Result<(), TestError> {
    let source = br"struct Node;
struct Node { int x; };";
    with_scratch(|scratch| {
        let facts = collect(
            ClangInput::Cxx {
                file_name: c"definition-preference.cc",
                source,
                standard: CxxStandard::Cxx23,
            },
            scratch,
        )?;
        let record_count = facts
            .declarations
            .iter()
            .filter(|fact| fact.kind == DeclarationKind::Record)
            .count();
        assert_eq!(record_count, 1);
        let record = facts
            .declarations
            .iter()
            .find(|fact| fact.kind == DeclarationKind::Record)
            .ok_or(TestError::Missing(RequiredFact::Declaration {
                kind: DeclarationKind::Record,
            }))?;
        assert_eq!(record.definition, DefinitionState::Definition);
        assert_eq!(
            record.name.map(|span| source_at(source, span)),
            Some(&b"Node"[..])
        );
        assert!(record.type_root.is_some());
        assert!(facts.declarations.iter().any(|fact| {
            fact.kind == DeclarationKind::Field
                && fact
                    .name
                    .is_some_and(|span| source_at(source, span) == b"x")
                && fact.owner == record.identity
        }));
        Ok(())
    })
}

#[test]
fn cxx_authority_projects_template_pattern_into_box_authority() -> Result<(), TestError> {
    let source = br"template<typename T> struct Box { T value; };
struct User { struct Box<int> box; };";
    with_scratch(|scratch| {
        let facts = collect(
            ClangInput::Cxx {
                file_name: c"template.cc",
                source,
                standard: CxxStandard::Cxx23,
            },
            scratch,
        )?;
        let template = facts
            .declarations
            .iter()
            .find(|fact| {
                fact.kind == DeclarationKind::Template
                    && fact
                        .name
                        .is_some_and(|span| source_at(source, span) == b"Box")
            })
            .copied();
        assert!(template.is_some());
        let template = template.ok_or(TestError::Missing(RequiredFact::Declaration {
            kind: DeclarationKind::Template,
        }))?;
        assert_eq!(template.definition, DefinitionState::Definition);
        let box_identity = template.identity;
        assert!(facts.declarations.iter().any(|fact| {
            fact.kind == DeclarationKind::TemplateParameter
                && fact
                    .name
                    .is_some_and(|span| source_at(source, span) == b"T")
                && fact.owner == box_identity
        }));
        assert!(facts.declarations.iter().any(|fact| {
            fact.kind == DeclarationKind::Field
                && fact
                    .name
                    .is_some_and(|span| source_at(source, span) == b"value")
                && fact.owner == box_identity
        }));
        assert!(facts.declarations.iter().any(|fact| {
            fact.kind == DeclarationKind::Record
                && fact
                    .name
                    .is_some_and(|span| source_at(source, span) == b"User")
        }));
        assert!(facts.declarations.iter().any(|fact| {
            fact.kind == DeclarationKind::Field
                && fact
                    .name
                    .is_some_and(|span| source_at(source, span) == b"box")
        }));
        assert!(!facts.declarations.iter().any(|fact| {
            fact.kind == DeclarationKind::Record
                && fact
                    .name
                    .is_some_and(|span| source_at(source, span) == b"Box")
        }));
        assert!(!facts.declarations.iter().any(|fact| {
            fact.kind == DeclarationKind::Field
                && fact
                    .name
                    .is_some_and(|span| source_at(source, span) == b"struct")
        }));
        Ok(())
    })
}

fn method_identity(
    facts: &backend_frontend_clang::legacy::ClangFacts<'_>,
    source: &[u8],
    name: &[u8],
    owner: &[u8],
    virtuality: MethodVirtuality,
) -> Result<SymbolIdentity, TestError> {
    facts
        .declarations
        .iter()
        .find(|declaration| {
            declaration.kind == DeclarationKind::Method
                && declaration.virtuality == virtuality
                && declaration
                    .name
                    .is_some_and(|span| source_at(source, span) == name)
                && declaration.owner.is_some_and(|identity| {
                    facts.declarations.iter().any(|owner_declaration| {
                        owner_declaration.identity == Some(identity)
                            && owner_declaration
                                .name
                                .is_some_and(|span| source_at(source, span) == owner)
                    })
                })
        })
        .and_then(|declaration| declaration.identity)
        .ok_or(TestError::Missing(RequiredFact::OverloadIdentity))
}

/// Builds enough caller-owned typed capacity for a small but deliberately rich native fixture.
fn with_scratch<Output>(
    run: impl for<'scratch> FnOnce(ClangScratch<'scratch>) -> Result<Output, TestError>,
) -> Result<Output, TestError> {
    let mut declarations = [empty_declaration(); DECLARATION_SLOTS];
    let mut types = [empty_type(); TYPE_SLOTS];
    let mut type_edges = [empty_type_edge(); TYPE_EDGE_SLOTS];
    let mut references = [empty_reference(); REFERENCE_SLOTS];
    let mut diagnostics = [empty_diagnostic(); DIAGNOSTIC_SLOTS];
    let mut includes = [empty_include(); DEPENDENCY_SLOTS];
    let mut overrides = [empty_override(); OVERRIDE_SLOTS];
    run(ClangScratch {
        declarations: &mut declarations,
        types: &mut types,
        type_edges: &mut type_edges,
        references: &mut references,
        diagnostics: &mut diagnostics,
        includes: &mut includes,
        overrides: &mut overrides,
    })
}

/// Requires two distinct USR-derived identities among C++ function declarations.
fn distinct_score_overload_identity(
    facts: &backend_frontend_clang::legacy::ClangFacts<'_>,
    source: &[u8],
) -> Result<(), TestError> {
    let mut first = None;
    for declaration in facts.declarations {
        if declaration.kind != DeclarationKind::Function
            || declaration
                .name
                .is_none_or(|span| source_at(source, span) != b"score")
        {
            continue;
        }
        let Some(identity) = declaration.identity else {
            continue;
        };
        if first.is_some_and(|observed| observed != identity) {
            return Ok(());
        }
        first = Some(identity);
    }
    Err(TestError::Missing(RequiredFact::OverloadIdentity))
}

const DECLARATION_SLOTS: usize = 32;
const TYPE_SLOTS: usize = 96;
const TYPE_EDGE_SLOTS: usize = 192;
const REFERENCE_SLOTS: usize = 64;
const DIAGNOSTIC_SLOTS: usize = 16;
const DEPENDENCY_SLOTS: usize = 16;
const OVERRIDE_SLOTS: usize = 16;

const fn empty_span() -> SourceSpan {
    SourceSpan { start: 0, end: 0 }
}

const fn empty_declaration() -> DeclarationFact {
    DeclarationFact {
        id: DeclarationId { raw: 0 },
        kind: DeclarationKind::Unknown,
        definition: DefinitionState::Declaration,
        virtuality: MethodVirtuality::NonVirtual,
        identity: None,
        span: empty_span(),
        name: None,
        owner: None,
        documentation: None,
        storage: StorageClass::None,
        type_root: None,
        enum_underlying: None,
    }
}

const fn empty_override() -> OverrideFact {
    OverrideFact {
        source: SymbolIdentity { bytes: [0; 16] },
        target: SymbolIdentity { bytes: [0; 16] },
        target_file: None,
    }
}

const fn empty_type() -> TypeFact {
    TypeFact {
        id: TypeId { raw: 0 },
        kind: TypeKind::Unknown,
        qualifiers: TypeQualifiers {
            is_const: false,
            is_volatile: false,
            is_restrict: false,
        },
        declaration: None,
        array_len: None,
        builtin: Some(BuiltinClass::Other),
        size_bits: None,
        align_bits: None,
        is_variadic: false,
    }
}

const fn empty_type_edge() -> TypeEdge {
    TypeEdge {
        source: TypeId { raw: 0 },
        relation: backend_frontend_clang::legacy::facts::TypeRelation::Pointee,
        target: TypeId { raw: 0 },
    }
}

const fn empty_reference() -> ReferenceFact {
    ReferenceFact {
        kind: ReferenceKind::Value,
        span: empty_span(),
        owner: None,
        target: ReferenceTarget::Unresolved,
    }
}

const fn empty_diagnostic() -> DiagnosticFact {
    DiagnosticFact {
        severity: backend_frontend_clang::legacy::facts::DiagnosticSeverity::Ignored,
        location: None,
        category: 0,
        message: None,
    }
}

const fn empty_include() -> IncludeFact {
    IncludeFact {
        kind: SourceDependencyKind::Include,
        span: empty_span(),
        resolved: None,
    }
}

/// Requires one declaration kind without inspecting native names or source scanner output.
fn require_declaration(
    facts: &backend_frontend_clang::legacy::ClangFacts<'_>,
    kind: DeclarationKind,
) -> Result<(), TestError> {
    facts
        .declarations
        .iter()
        .any(|declaration| declaration.kind == kind)
        .then_some(())
        .ok_or(TestError::Missing(RequiredFact::Declaration { kind }))
}

/// Requires one type kind from the directly emitted recursive type graph.
fn require_type(
    facts: &backend_frontend_clang::legacy::ClangFacts<'_>,
    kind: TypeKind,
) -> Result<(), TestError> {
    facts
        .types
        .iter()
        .any(|type_fact| type_fact.kind == kind)
        .then_some(())
        .ok_or(TestError::Missing(RequiredFact::Type { kind }))
}

/// Requires one direct relation from the recursive native type graph.
fn require_type_relation(
    facts: &backend_frontend_clang::legacy::ClangFacts<'_>,
    relation: backend_frontend_clang::legacy::TypeRelation,
) -> Result<(), TestError> {
    facts
        .type_edges
        .iter()
        .any(|edge| edge.relation == relation)
        .then_some(())
        .ok_or(TestError::Missing(RequiredFact::TypeRelation { relation }))
}

/// Requires one direct include or module dependency fact.
fn require_dependency(
    facts: &backend_frontend_clang::legacy::ClangFacts<'_>,
    kind: SourceDependencyKind,
) -> Result<(), TestError> {
    facts
        .includes
        .iter()
        .any(|dependency| dependency.kind == kind)
        .then_some(())
        .ok_or(TestError::Missing(RequiredFact::Dependency { kind }))
}

/// Requires one call expression whose direct libclang target resolves inside the main source.
fn require_local_call(
    facts: &backend_frontend_clang::legacy::ClangFacts<'_>,
) -> Result<(), TestError> {
    facts
        .references
        .iter()
        .any(|reference| {
            reference.kind == ReferenceKind::Call
                && matches!(reference.target, ReferenceTarget::Local(_))
        })
        .then_some(())
        .ok_or(TestError::Missing(RequiredFact::LocalCall))
}

/// Diagnostic: drive the exact engine project lane over a real corpus package
/// and print parse/collect timing and fact counts. Skipped unless
/// `NUDOX_CLANG_CORPUS_DIR` and `NUDOX_CLANG_DIAG_PACKAGE` are both set.
#[test]
fn diagnostic_corpus_project_lane() -> Result<(), TestError> {
    let Ok(corpus) = std::env::var("NUDOX_CLANG_CORPUS_DIR") else {
        return Ok(());
    };
    let Ok(package) = std::env::var("NUDOX_CLANG_DIAG_PACKAGE") else {
        return Ok(());
    };
    let root = std::path::PathBuf::from(corpus).join(&package);
    let mut files: Vec<(std::path::PathBuf, u64)> = Vec::new();
    collect_sources(&root, &mut files)?;
    files.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    let entry = files
        .first()
        .map(|(path, _)| path.clone())
        .ok_or(TestError::Missing(RequiredFact::Declaration {
            kind: DeclarationKind::Function,
        }))?;
    let source = fs::read(&entry).map_err(|_| {
        TestError::Missing(RequiredFact::Declaration {
            kind: DeclarationKind::Function,
        })
    })?;
    let project = backend_frontend_clang::ClangProject::open(&root, &entry).map_err(|_| {
        TestError::Missing(RequiredFact::Declaration {
            kind: DeclarationKind::Function,
        })
    })?;
    println!("diagnostic entry={entry:?} bytes={}", source.len());
    let arguments = project.arguments();
    let owned = arguments
        .iter()
        .map(|argument| std::ffi::CString::new(argument.as_bytes()).expect("no interior NUL"))
        .collect::<Vec<_>>();
    let borrowed = owned
        .iter()
        .map(std::ffi::CString::as_c_str)
        .collect::<Vec<_>>();
    let file_name = std::ffi::CString::new(project.entry().to_string_lossy().as_bytes())
        .expect("no interior NUL");
    let directory = std::ffi::CString::new(project.root().to_string_lossy().as_bytes())
        .expect("no interior NUL");
    let mut declarations = vec![empty_declaration(); MAX_CLANG_DECLARATIONS];
    let mut types = vec![empty_type(); MAX_CLANG_TYPES];
    let mut type_edges = vec![empty_type_edge(); MAX_CLANG_TYPE_EDGES];
    let mut references = vec![empty_reference(); MAX_CLANG_REFERENCES];
    let mut diagnostics = vec![empty_diagnostic(); MAX_CLANG_DIAGNOSTICS];
    let mut includes = vec![empty_include(); MAX_CLANG_INCLUDES];
    let mut overrides = vec![empty_override(); MAX_CLANG_OVERRIDES];
    let started = std::time::Instant::now();
    let outcome = collect(
        ClangInput::from_database(&file_name, &source, &borrowed, &directory).map_err(|_| {
            TestError::Missing(RequiredFact::Declaration {
                kind: DeclarationKind::Function,
            })
        })?,
        ClangScratch {
            declarations: &mut declarations,
            types: &mut types,
            type_edges: &mut type_edges,
            references: &mut references,
            diagnostics: &mut diagnostics,
            includes: &mut includes,
            overrides: &mut overrides,
        },
    );
    println!("diagnostic elapsed={:?}", started.elapsed());
    match outcome {
        Ok(facts) => println!(
            "diagnostic ok declarations={} types={} edges={} references={} includes={} diagnostics={} overrides={}",
            facts.declarations.len(),
            facts.types.len(),
            facts.type_edges.len(),
            facts.references.len(),
            facts.includes.len(),
            facts.diagnostics.len(),
            facts.overrides.len(),
        ),
        Err(error) => println!("diagnostic error={error:?}"),
    }
    Ok(())
}

fn collect_sources(
    root: &std::path::Path,
    files: &mut Vec<(std::path::PathBuf, u64)>,
) -> Result<(), TestError> {
    for entry in fs::read_dir(root).map_err(|_| {
        TestError::Missing(RequiredFact::Declaration {
            kind: DeclarationKind::Function,
        })
    })? {
        let entry = entry.map_err(|_| {
            TestError::Missing(RequiredFact::Declaration {
                kind: DeclarationKind::Function,
            })
        })?;
        let path = entry.path();
        let metadata = entry.metadata().map_err(|_| {
            TestError::Missing(RequiredFact::Declaration {
                kind: DeclarationKind::Function,
            })
        })?;
        if metadata.is_dir() {
            collect_sources(&path, files)?;
        } else if metadata.is_file()
            && path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    matches!(extension, "c" | "cc" | "cpp" | "cxx" | "C" | "hpp" | "h")
                })
        {
            files.push((path, metadata.len()));
        }
    }
    Ok(())
}

/// Borrows one proven source span without permitting invalid native coordinates to panic a test.
fn source_at(source: &[u8], span: SourceSpan) -> &[u8] {
    let Ok(start) = usize::try_from(span.start) else {
        return &[];
    };
    let Ok(end) = usize::try_from(span.end) else {
        return &[];
    };
    match source.get(start..end) {
        Some(bytes) => bytes,
        None => &[],
    }
}
