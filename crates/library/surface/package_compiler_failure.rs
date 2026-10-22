//! Closed, source-bound package compiler refusal facts.
//!
//! These summaries intentionally omit native diagnostic bytes and error-chain
//! strings. They retain the exact package member/source authority and a closed
//! cause vocabulary suitable for durable receipts and CLI/MCP presentation.

use crate::interface::{
    AuthorityDiagnosticClass, AuthorityPhase, CompilerCause, CompilerFragmentFaultFacts,
    CompilerFragmentFaultKind, CompilerFragmentFaultPhase, CompilerTerminal, CompilerToolFailure,
    CompilerToolIssue, CompilerToolRequirement, PythonAuthorityFailureKind,
};
use backend_semantic::vocabulary::{
    ClangProjectionFault, GoProjectionFault, JavaProjectionFault, LoweringUnsupported, NativeTool,
    ProjectionAdmissionFault, ProjectionAnonymousCallableAnchorFault,
    ProjectionAnonymousCallableAnchorPool, ProjectionConstructorFault, ProjectionConstructorTag,
    ProjectionSemanticTypeFault, ProjectionSemanticTypeTag, ProjectionTypeCell,
    PythonProjectionFault, TypeScriptProjectionFault,
};
use backend_version::{CompileRecipeDomain, ContentId};
use serde::{Deserialize, Serialize};

/// Phase of a package compiler refusal. Fragment phases are derived from the
/// concrete fragment cause; setup, lowering, and authority phases are closed
/// siblings and cannot be paired with a fragment fault.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageCompilerFailurePhase {
    /// Compact-fragment preparation failed before native compilation.
    Prepare,
    /// Writing the selected compact fragment failed.
    Write,
    /// Validation of the written compact fragment failed.
    Validate,
    /// A required tool or pre-recipe authority setup step failed.
    Setup,
    /// The accepted source could not be lowered to the selected closed recipe.
    Lowering,
    /// The compiler authority rejected the source or its semantic projection.
    Authority,
}

/// Closed language fact retained for pre-recipe toolchain refusals.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompilerLanguageFact {
    /// Rust source language.
    Rust,
    /// TypeScript source language.
    TypeScript,
    /// Python source language.
    Python,
    /// Go source language.
    Go,
    /// Java source language.
    Java,
    /// C# source language.
    CSharp,
    /// Clang-family source language.
    Clang,
}

/// Closed compiler stage fact retained for pre-recipe toolchain refusals.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CompilerStageFact {
    /// The compiler parse stage.
    Parse,
    /// The compiler lowering-to-IR stage.
    LowerIr,
}

/// Closed native executable family with the host's actual configuration knob.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompilerNativeToolFact {
    /// The Rust compiler executable.
    Rustc,
    /// The Clang compiler executable.
    Clang,
    /// The Python interpreter executable.
    Python,
    /// The TypeScript compiler executable.
    TypeScriptCompiler,
    /// The Go compiler executable.
    GoCompiler,
    /// The Java compiler executable.
    JavaCompiler,
    /// The .NET compiler executable.
    CSharpCompiler,
}

impl CompilerNativeToolFact {
    /// Executable selected by the closed compiler registry.
    #[must_use]
    pub const fn executable(self) -> &'static str {
        match self {
            Self::Rustc => "rustc",
            Self::Clang => "clang",
            Self::Python => "python3",
            Self::TypeScriptCompiler => "tsc",
            Self::GoCompiler => "go",
            Self::JavaCompiler => "javac",
            Self::CSharpCompiler => "dotnet",
        }
    }

    /// Environment variable consumed by the local host's configured-tool adapter.
    #[must_use]
    pub const fn configuration_variable(self) -> &'static str {
        match self {
            Self::Rustc => "NUDOX_RUSTC",
            Self::Clang => "NUDOX_CLANG",
            Self::Python => "NUDOX_PYTHON",
            Self::TypeScriptCompiler => "NUDOX_TSC",
            Self::GoCompiler => "NUDOX_GO",
            Self::JavaCompiler => "NUDOX_JAVAC",
            Self::CSharpCompiler => "NUDOX_DOTNET",
        }
    }

    /// Conventional project-local executable location, where this ecosystem defines one.
    #[must_use]
    pub const fn project_local_path(self) -> Option<&'static str> {
        match self {
            Self::TypeScriptCompiler => Some("node_modules/.bin/tsc"),
            Self::Python => Some(".venv/bin/python"),
            Self::Rustc
            | Self::Clang
            | Self::GoCompiler
            | Self::JavaCompiler
            | Self::CSharpCompiler => None,
        }
    }
}

/// Closed, exact compiler failure family. The family itself determines phase
/// and kind tag; the wire carries no independently writable phase/tag pair.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    tag = "family",
    content = "fault",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum PackageCompilerFailureCause {
    /// Exact compact-fragment family and its required operand facts.
    Fragment(PackageCompilerFragmentFaultFacts),
    /// A registry-selected native tool did not match the configured tool.
    Toolchain {
        /// Source language whose registry entry selected the tool.
        language: CompilerLanguageFact,
        /// Compiler stage for which the tool was selected.
        stage: CompilerStageFact,
        /// Executable family selected by the compiler registry.
        selected: CompilerNativeToolFact,
        /// Executable family selected by host configuration, when one was configured.
        configured: Option<CompilerNativeToolFact>,
    },
    /// The registry-selected native tool is explicitly absent.
    ToolingUnavailable {
        /// Source language whose registry entry requires the absent tool.
        language: CompilerLanguageFact,
        /// Compiler stage blocked by the absent tool.
        stage: CompilerStageFact,
        /// Registry-selected executable family that could not be found.
        tool: CompilerNativeToolFact,
    },
    /// One exact native or auxiliary tool requirement is absent or failed its bounded probe.
    RequiredTool {
        /// Source language whose compiler setup requires the tool.
        language: CompilerLanguageFact,
        /// Compiler stage blocked by the tool requirement.
        stage: CompilerStageFact,
        /// Exact native or auxiliary requirement and its missing or failed-probe state.
        issue: CompilerToolIssue,
    },
    /// Syntax was accepted but the exact closed lowering recipe was unavailable.
    Lowering(PackageLoweringFaultFacts),
    /// A concrete authority admission failed before a compiler recipe existed.
    AuthorityAdmission {
        /// Source language whose pre-recipe authority admission failed.
        language: CompilerLanguageFact,
        /// Compiler stage for which authority admission was attempted.
        stage: CompilerStageFact,
        /// Authority transaction phase that failed.
        phase: AuthorityPhaseFact,
        /// Kind of authority rejection reported for the failed phase.
        class: AuthorityClassFact,
        /// Bounded diagnostic facts retained for the authority rejection, when available.
        diagnostic: Option<CompilerAuthorityDiagnosticFacts>,
    },
    /// Existing compiler authority rejected the source or its semantic projection.
    Authority {
        /// Authority transaction phase that rejected the compiled source.
        phase: AuthorityPhaseFact,
        /// Kind of authority rejection reported for the failed phase.
        class: AuthorityClassFact,
        /// Bounded diagnostic facts retained for the authority rejection, when available.
        diagnostic: Option<CompilerAuthorityDiagnosticFacts>,
    },
}

impl PackageCompilerFailureCause {
    /// Phase implied by this exact closed cause family.
    #[must_use]
    pub const fn phase(&self) -> PackageCompilerFailurePhase {
        match self {
            Self::Fragment(fault) => match fault.kind().phase() {
                CompilerFragmentFaultPhase::Prepare => PackageCompilerFailurePhase::Prepare,
                CompilerFragmentFaultPhase::Write => PackageCompilerFailurePhase::Write,
                CompilerFragmentFaultPhase::Validate => PackageCompilerFailurePhase::Validate,
            },
            Self::Toolchain { .. }
            | Self::ToolingUnavailable { .. }
            | Self::RequiredTool { .. }
            | Self::AuthorityAdmission { .. } => PackageCompilerFailurePhase::Setup,
            Self::Lowering(_) => PackageCompilerFailurePhase::Lowering,
            Self::Authority { .. } => PackageCompilerFailurePhase::Authority,
        }
    }

    /// Stable, specific cause tag derived from the closed fault variant.
    #[must_use]
    pub const fn kind_tag(&self) -> &'static str {
        match self {
            Self::Fragment(fault) => fault.kind().tag(),
            Self::Toolchain { .. } => "toolchain_configuration_mismatch",
            Self::ToolingUnavailable { .. } => "tooling_unavailable",
            Self::RequiredTool {
                issue:
                    CompilerToolIssue {
                        failure: CompilerToolFailure::Missing,
                        ..
                    },
                ..
            } => "required_tool_missing",
            Self::RequiredTool {
                issue:
                    CompilerToolIssue {
                        failure: CompilerToolFailure::ProbeFailed,
                        ..
                    },
                ..
            } => "required_tool_probe_failed",
            Self::Lowering(fault) => fault.kind_tag(),
            Self::Authority {
                class: _,
                diagnostic:
                    Some(CompilerAuthorityDiagnosticFacts {
                        python_failure: Some(failure),
                        ..
                    }),
                ..
            }
            | Self::AuthorityAdmission {
                class: _,
                diagnostic:
                    Some(CompilerAuthorityDiagnosticFacts {
                        python_failure: Some(failure),
                        ..
                    }),
                ..
            } => failure.kind_tag(),
            Self::Authority {
                diagnostic:
                    Some(CompilerAuthorityDiagnosticFacts {
                        typescript_failure: Some(failure),
                        ..
                    }),
                ..
            }
            | Self::AuthorityAdmission {
                diagnostic:
                    Some(CompilerAuthorityDiagnosticFacts {
                        typescript_failure: Some(failure),
                        ..
                    }),
                ..
            } => failure.kind_tag(),
            Self::Authority {
                diagnostic:
                    Some(CompilerAuthorityDiagnosticFacts {
                        go_failure: Some(failure),
                        ..
                    }),
                ..
            }
            | Self::AuthorityAdmission {
                diagnostic:
                    Some(CompilerAuthorityDiagnosticFacts {
                        go_failure: Some(failure),
                        ..
                    }),
                ..
            } => failure.kind_tag(),
            Self::Authority {
                diagnostic:
                    Some(CompilerAuthorityDiagnosticFacts {
                        rust_source_scope_failure: Some(_),
                        ..
                    }),
                ..
            }
            | Self::AuthorityAdmission {
                diagnostic:
                    Some(CompilerAuthorityDiagnosticFacts {
                        rust_source_scope_failure: Some(_),
                        ..
                    }),
                ..
            } => "rust_source_scope_failure",
            Self::Authority { class, .. } | Self::AuthorityAdmission { class, .. } => match class {
                AuthorityClassFact::Syntax => "authority_syntax",
                AuthorityClassFact::Binding => "authority_binding",
                AuthorityClassFact::SourceScope => "authority_source_scope",
                AuthorityClassFact::Type => "authority_type",
                AuthorityClassFact::Authority => "authority_runtime",
                AuthorityClassFact::Projection => "authority_projection",
            },
        }
    }

    /// Native executable selected by this failure, if setup was the terminal.
    #[must_use]
    pub const fn required_native_tool(&self) -> Option<CompilerNativeToolFact> {
        match self {
            Self::Toolchain { selected, .. } => Some(*selected),
            Self::ToolingUnavailable { tool, .. } => Some(*tool),
            Self::RequiredTool {
                issue:
                    CompilerToolIssue {
                        requirement: CompilerToolRequirement::Native(tool),
                        ..
                    },
                ..
            } => Some(match tool {
                NativeTool::Rustc => CompilerNativeToolFact::Rustc,
                NativeTool::Clang => CompilerNativeToolFact::Clang,
                NativeTool::Python => CompilerNativeToolFact::Python,
                NativeTool::TypeScriptCompiler => CompilerNativeToolFact::TypeScriptCompiler,
                NativeTool::GoCompiler => CompilerNativeToolFact::GoCompiler,
                NativeTool::JavaCompiler => CompilerNativeToolFact::JavaCompiler,
                NativeTool::CSharpCompiler => CompilerNativeToolFact::CSharpCompiler,
            }),
            _ => None,
        }
    }

    /// Configured executable family when a setup mismatch has one.
    #[must_use]
    pub const fn configured_native_tool(&self) -> Option<CompilerNativeToolFact> {
        match self {
            Self::Toolchain { configured, .. } => *configured,
            _ => None,
        }
    }

    /// Exact setup issue when a separately typed required tool blocked admission.
    #[must_use]
    pub const fn required_tool_issue(&self) -> Option<CompilerToolIssue> {
        match self {
            Self::RequiredTool { issue, .. } => Some(*issue),
            _ => None,
        }
    }

    /// Exact host configuration variable that remedies this required-tool failure.
    #[must_use]
    pub const fn required_configuration_variable(&self) -> Option<&'static str> {
        match self {
            Self::RequiredTool { issue, .. } => Some(issue.configuration_variable()),
            Self::Toolchain { selected, .. } | Self::ToolingUnavailable { tool: selected, .. } => {
                Some(selected.configuration_variable())
            }
            _ => None,
        }
    }

    /// Whether a selected native or auxiliary tool must be configured or repaired before retrying.
    #[must_use]
    pub const fn requires_tool_configuration(&self) -> bool {
        matches!(
            self,
            Self::Toolchain { .. } | Self::ToolingUnavailable { .. } | Self::RequiredTool { .. }
        )
    }
}

/// Checked pair of a compact-fragment kind and its matching bounded facts.
///
/// The kind and facts serialize as the established `kind` and `facts` members,
/// but are validated together on construction and deserialization.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageCompilerFragmentFaultFacts {
    kind: CompilerFragmentFaultKind,
    facts: CompilerFragmentFaultFacts,
}

impl PackageCompilerFragmentFaultFacts {
    /// Constructs a checked kind/facts pair.
    ///
    /// # Errors
    ///
    /// Returns an admission error when the exact fact variant does not belong
    /// to the supplied kind.
    pub fn new(
        kind: CompilerFragmentFaultKind,
        facts: CompilerFragmentFaultFacts,
    ) -> Result<Self, super::ProductAdmissionError> {
        if !super::compiler_fault_facts_match_kind(kind, facts) {
            return Err(super::ProductAdmissionError::PackageCompilerFailureShape);
        }
        Ok(Self { kind, facts })
    }

    /// Exact closed fragment error kind.
    #[must_use]
    pub const fn kind(self) -> CompilerFragmentFaultKind {
        self.kind
    }

    /// Required bounded operands retained for the exact error kind.
    #[must_use]
    pub const fn facts(self) -> CompilerFragmentFaultFacts {
        self.facts
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageCompilerFragmentFaultFactsWire {
    kind: CompilerFragmentFaultKind,
    facts: CompilerFragmentFaultFacts,
}

impl<'de> Deserialize<'de> for PackageCompilerFragmentFaultFacts {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;

        let wire = PackageCompilerFragmentFaultFactsWire::deserialize(deserializer)?;
        Self::new(wire.kind, wire.facts).map_err(D::Error::custom)
    }
}

/// Closed authority transaction phase.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityPhaseFact {
    /// Opening the selected project, native library, or authority image failed.
    Open,
    /// Parsing the selected source or authority image failed.
    Parse,
    /// Cross-file, symbol, import, or package resolution failed.
    Resolve,
    /// Type checking or recursive type extraction failed.
    TypeCheck,
    /// Projecting valid authority facts into canonical IR failed.
    Project,
}

/// Closed authority diagnostic class.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityClassFact {
    /// The authority rejected source or image syntax.
    Syntax,
    /// A symbol, import, package, or project-graph binding could not be established.
    Binding,
    /// A selected source lies outside the active semantic scope.
    SourceScope,
    /// A required type fact could not be established.
    Type,
    /// Loading or running the selected authority failed.
    Authority,
    /// Canonical admission rejected the authority's complete fact set.
    Projection,
}

/// Safe authority-diagnostic extent; native bytes are deliberately omitted.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CompilerAuthorityDiagnosticFacts {
    /// Bytes retained in the bounded diagnostic prefix.
    pub retained_bytes: u32,
    /// Total source-diagnostic bytes observed before bounded retention.
    pub observed_bytes: u64,
    /// Whether the retained prefix omits observed diagnostic bytes.
    pub truncated: bool,
    /// Closed Python checker cause when available. Native diagnostic text and
    /// paths remain local-only and are not serialized into this summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python_failure: Option<PythonAuthorityFailureKind>,
    /// Source identity and omission facts only; no config path, bytes or values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python_configuration: Option<crate::interface::PythonConfigurationRefusalFacts>,
    /// Exact closed TypeScript cause; transcript, paths and solver text remain local.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub typescript_failure: Option<crate::interface::TypeScriptAuthorityFailureKind>,
    /// Where a native TypeScript program was when it stopped: its innermost
    /// boundary, exact program path, and admitted work. Present only for a
    /// program execution stop that observed a boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub typescript_stop_site: Option<crate::interface::TypeScriptStopSite>,
    /// Typed compiler version, bounded counts and exact missing API capability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub typescript_admission: Option<crate::interface::TypeScriptCompilerAdmissionDetail>,
    /// Bounded, sanitized host cause text (project-relative paths, no native output).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub typescript_cause: Option<crate::interface::TypeScriptFailureCause>,
    /// Exact Go dependency-admission cause; native diagnostic text stays local.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub go_failure: Option<crate::interface::GoAuthorityFailureKind>,
    /// Exact path-free Rust source-scope kind and bounded authority counts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rust_source_scope_failure: Option<crate::interface::RustSourceScopeFailureKind>,
}

/// Closed compact-lowering cause, with exact TypeScript projection operands
/// and an exhaustive tag for every other existing lowering terminal.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PackageLoweringFaultFacts {
    /// No declaration in the source has a supported lowering recipe.
    NoSupportedDeclaration,
    /// A row refers to a provisional extension atom outside the bound atom table.
    ExtensionAtomUnbound {
        /// Zero-based emission row that named the unbound atom.
        row: u32,
        /// Provisional atom coordinate named by the fact.
        provisional: u32,
        /// Number of atoms actually held by the admitted lane.
        atom_count: u32,
    },
    /// A row refers to a type-parameter slice outside the bound type-parameter table.
    ExtensionTypeParametersUnbound {
        /// Zero-based extension row that named the unbound interval.
        row: u32,
        /// Provisional index of the first type-parameter element.
        start: u32,
        /// Number of provisional type-parameter elements in the interval.
        length: u32,
        /// Number of elements actually held by the admitted type-parameter lane.
        element_count: u32,
    },
    /// A projected fact was rejected during canonical admission.
    FactRejected {
        /// Zero-based ordinal the fact would have occupied.
        fact: u64,
        /// Byte length of the rejected declaration name.
        name_len: u64,
        /// Closed cause carrying the details for this fault.
        cause: PackageProjectionAdmissionFaultFacts,
    },
    /// The Rust function declaration form is unsupported by the selected recipe.
    RustFunction,
    /// The Rust constant has a type form the selected recipe cannot lower.
    RustConstantType,
    /// The Rust generic parameter form is unsupported by the selected recipe.
    RustGenericParameter,
    /// The Python assignment target cannot be represented as a declaration name.
    PythonAssignmentName,
    /// The Python assignment value has no supported declaration lowering.
    PythonAssignmentValue,
    /// The Clang declaration form is unsupported by the selected recipe.
    ClangDeclarationForm,
    /// The TypeScript declaration form is unsupported by the selected recipe.
    TypeScriptDeclarationForm,
    /// The TypeScript declaration type has no supported lowering.
    TypeScriptDeclarationType,
    /// The C# declaration form is unsupported by the selected recipe.
    CSharpDeclarationForm,
    /// The C# declaration type has no supported lowering.
    CSharpDeclarationType,
    /// The Go declaration form is unsupported by the selected recipe.
    GoDeclarationForm,
    /// The Go declaration type has no supported lowering.
    GoDeclarationType,
    /// The Java declaration form is unsupported by the selected recipe.
    JavaDeclarationForm,
    /// The Java authority projection rejected a fact while lowering.
    JavaProjection {
        /// Closed language-projection fault retained for this lowering refusal.
        fault: PackageLanguageProjectionFault,
    },
    /// The Go authority projection rejected a fact while lowering.
    GoProjection {
        /// Closed language-projection fault retained for this lowering refusal.
        fault: PackageLanguageProjectionFault,
    },
    /// The TypeScript authority projection rejected a fact while lowering.
    TypeScriptProjection {
        /// Closed language-projection fault retained for this lowering refusal.
        fault: PackageTypeScriptProjectionFaultFacts,
    },
    /// The Python authority projection rejected a fact while lowering.
    PythonProjection {
        /// Closed language-projection fault retained for this lowering refusal.
        fault: PackageLanguageProjectionFault,
    },
    /// The C# authority projection rejected a fact while lowering.
    CSharpProjection {
        /// Closed language-projection fault retained for this lowering refusal.
        fault: PackageLanguageProjectionFault,
    },
    /// The Clang authority projection rejected a fact while lowering.
    ClangProjection {
        /// Closed language-projection fault retained for this lowering refusal.
        fault: PackageLanguageProjectionFault,
    },
}

impl PackageLoweringFaultFacts {
    /// Stable specific cause tag, derived from the closed lowering variant.
    #[must_use]
    pub const fn kind_tag(self) -> &'static str {
        match self {
            Self::NoSupportedDeclaration => "lowering_no_supported_declaration",
            Self::ExtensionAtomUnbound { .. } => "lowering_extension_atom_unbound",
            Self::ExtensionTypeParametersUnbound { .. } => {
                "lowering_extension_type_parameters_unbound"
            }
            Self::FactRejected { cause, .. } => cause.kind_tag(),
            Self::RustFunction => "lowering_rust_function",
            Self::RustConstantType => "lowering_rust_constant_type",
            Self::RustGenericParameter => "lowering_rust_generic_parameter",
            Self::PythonAssignmentName => "lowering_python_assignment_name",
            Self::PythonAssignmentValue => "lowering_python_assignment_value",
            Self::ClangDeclarationForm => "lowering_clang_declaration_form",
            Self::TypeScriptDeclarationForm => "lowering_typescript_declaration_form",
            Self::TypeScriptDeclarationType => "lowering_typescript_declaration_type",
            Self::CSharpDeclarationForm => "lowering_csharp_declaration_form",
            Self::CSharpDeclarationType => "lowering_csharp_declaration_type",
            Self::GoDeclarationForm => "lowering_go_declaration_form",
            Self::GoDeclarationType => "lowering_go_declaration_type",
            Self::JavaDeclarationForm => "lowering_java_declaration_form",
            Self::JavaProjection { fault } => fault.kind_tag(),
            Self::GoProjection { fault } => fault.kind_tag(),
            Self::TypeScriptProjection { fault } => fault.kind_tag(),
            Self::PythonProjection { fault } => fault.kind_tag(),
            Self::CSharpProjection { fault } => fault.kind_tag(),
            Self::ClangProjection { fault } => fault.kind_tag(),
        }
    }
}

/// Exact TypeScript authority projection fault and its named source operands.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "fault", rename_all = "snake_case", deny_unknown_fields)]
pub enum PackageTypeScriptProjectionFaultFacts {
    /// A TypeScript foreign key has an invalid package path.
    ForeignKey {
        /// Inclusive source-byte offset of the rejected foreign-key spelling.
        start: u32,
        /// Exclusive source-byte offset of the rejected foreign-key spelling.
        end: u32,
        /// Closed cause carrying the details for this fault.
        cause: PackageForeignKeyFaultFacts,
    },
    /// The TypeScript package lineage violates its closed grammar.
    PackageLineage {
        /// Inclusive source-byte offset of the rejected package-lineage spelling.
        start: u32,
        /// Exclusive source-byte offset of the rejected package-lineage spelling.
        end: u32,
        /// Closed cause carrying the details for this fault.
        cause: PackageLineageFaultFacts,
    },
    /// A TypeScript source coordinate exceeds the supported integer range.
    CoordinateOverflow {
        /// Host-sized source coordinate that could not fit the wire's `u32` coordinate.
        value: u64,
    },
    /// A TypeScript import fact has no matching binding.
    MissingImportBinding {
        /// Zero-based projected import fact with no resolved binding in the authority image.
        fact: u32,
    },
}

impl PackageTypeScriptProjectionFaultFacts {
    const fn kind_tag(self) -> &'static str {
        match self {
            Self::ForeignKey {
                cause: PackageForeignKeyFaultFacts::EmptyPath,
                ..
            } => "lowering_typescript_foreign_key_empty_path",
            Self::ForeignKey {
                cause: PackageForeignKeyFaultFacts::BackslashInPath,
                ..
            } => "lowering_typescript_foreign_key_backslash",
            Self::PackageLineage {
                cause: PackageLineageFaultFacts::EmptyEcosystem,
                ..
            } => "lowering_typescript_lineage_empty_ecosystem",
            Self::PackageLineage {
                cause: PackageLineageFaultFacts::EmptyPackage,
                ..
            } => "lowering_typescript_lineage_empty_package",
            Self::PackageLineage {
                cause: PackageLineageFaultFacts::SeparatorInEcosystem,
                ..
            } => "lowering_typescript_lineage_separator_ecosystem",
            Self::PackageLineage {
                cause: PackageLineageFaultFacts::SeparatorInPackage,
                ..
            } => "lowering_typescript_lineage_separator_package",
            Self::PackageLineage {
                cause: PackageLineageFaultFacts::BackslashEcosystem,
                ..
            } => "lowering_typescript_lineage_backslash_ecosystem",
            Self::PackageLineage {
                cause: PackageLineageFaultFacts::BackslashPackage,
                ..
            } => "lowering_typescript_lineage_backslash_package",
            Self::PackageLineage {
                cause: PackageLineageFaultFacts::BackslashInvalid { .. },
                ..
            } => "lowering_typescript_lineage_backslash_component",
            Self::CoordinateOverflow { .. } => "lowering_typescript_coordinate_overflow",
            Self::MissingImportBinding { .. } => "lowering_typescript_missing_import_binding",
        }
    }
}

/// Exact closed vocabulary for non-TypeScript language-projection tags.
/// Current projections retain these specific tags, while only the TypeScript
/// and shared admission projections retain all nested coordinates and facts.
/// Source spellings and opaque authority identities remain omitted.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageLanguageProjectionFault {
    /// The Java authority image failed validation.
    JavaImage,
    /// The Java type projection exceeded its supported depth.
    JavaDepth,
    /// A Java type record has a malformed representation.
    JavaMalformedType,
    /// A Java primitive type could not be represented.
    JavaPrimitive,
    /// A Java name atom is not valid UTF-8.
    JavaAtomUtf8,
    /// The Java source bytes are not valid UTF-8.
    JavaSourceUtf8,
    /// A Java UTF-16 offset could not be mapped to a source byte offset.
    JavaUtf16Offset,
    /// A Java UTF-16 range could not be mapped to a source byte range.
    JavaUtf16Range,
    /// A Java projected record has no represented owner.
    JavaOrphanOwner,
    /// A Java foreign member key is invalid.
    JavaForeignKey,
    /// The Java projection exceeded its sibling capacity.
    JavaSiblingCapacity,
    /// The Java projection index exceeded its supported capacity.
    JavaIndexCapacity,
    /// The Go authority image failed validation.
    GoImage,
    /// The Go projection index exceeded its supported capacity.
    GoIndexCapacity,
    /// The Go type projection exceeded its supported depth.
    GoDepth,
    /// A Go variadic function has no corresponding parameter.
    GoVariadicWithoutParameter,
    /// A Go source anchor could not be represented.
    GoAnchor,
    /// A Go projected list exceeded its supported capacity.
    GoListCapacity,
    /// A Go projected reference targets no represented fact.
    GoOrphanTarget,
    /// A Go foreign member key is invalid.
    GoForeignKey,
    /// The Go package lineage does not satisfy the package grammar.
    GoPackageLineage,
    /// A Go name atom is not valid UTF-8.
    GoAtomUtf8,
    /// A Go source span cannot be represented relative to the selected source.
    GoRelativeSpan,
    /// A Go projected record has no represented owner.
    GoOrphanOwner,
    /// The Go projection failed canonical admission.
    GoAdmission,
    /// A Python foreign spelling is not valid UTF-8.
    PythonForeignSpellingUtf8,
    /// A Python foreign member key is invalid.
    PythonForeignKey,
    /// The Python package lineage does not satisfy the package grammar.
    PythonPackageLineage,
    /// A Python projected source span violates containment.
    PythonContainment,
    /// The C# authority image failed validation.
    CSharpImage,
    /// The C# type projection exceeded its supported depth.
    CSharpDepth,
    /// A C# declaration name span is invalid.
    CSharpNameSpan,
    /// C# projected records are not in the required owner order.
    CSharpOwnerOrder,
    /// A C# source anchor could not be represented.
    CSharpAnchor,
    /// A C# foreign member reference is invalid.
    CSharpForeign,
    /// The C# projection exceeded its attribute capacity.
    CSharpAttributeCapacity,
    /// The C# projection index exceeded its supported capacity.
    CSharpIndexCapacity,
    /// C# array dimensions have inconsistent ranks.
    CSharpHeterogeneousArrayRank,
    /// A C# result name cannot be represented.
    CSharpResultName,
    /// The C# projection failed canonical admission.
    CSharpAdmission,
    /// A Clang source span is invalid.
    ClangSpan,
    /// A Clang declaration has no representable name.
    ClangNameless,
    /// A Clang source anchor could not be represented.
    ClangAnchor,
    /// The Clang projection index exceeded its supported capacity.
    ClangIndexCapacity,
    /// A Clang foreign override reference is invalid.
    ClangForeignOverride,
    /// A Clang foreign reference is invalid.
    ClangForeignReference,
    /// A Clang qualifier targets an unsupported type form.
    ClangIllegalQualifierTarget,
    /// A Clang member pointer has an unsupported owner type.
    ClangIllegalMemberPointerOwner,
}

impl PackageLanguageProjectionFault {
    const fn kind_tag(self) -> &'static str {
        match self {
            Self::JavaImage => "lowering_java_projection_image",
            Self::JavaDepth => "lowering_java_projection_depth",
            Self::JavaMalformedType => "lowering_java_projection_malformed_type",
            Self::JavaPrimitive => "lowering_java_projection_primitive",
            Self::JavaAtomUtf8 => "lowering_java_projection_atom_utf8",
            Self::JavaSourceUtf8 => "lowering_java_projection_source_utf8",
            Self::JavaUtf16Offset => "lowering_java_projection_utf16_offset",
            Self::JavaUtf16Range => "lowering_java_projection_utf16_range",
            Self::JavaOrphanOwner => "lowering_java_projection_orphan_owner",
            Self::JavaForeignKey => "lowering_java_projection_foreign_key",
            Self::JavaSiblingCapacity => "lowering_java_projection_sibling_capacity",
            Self::JavaIndexCapacity => "lowering_java_projection_index_capacity",
            Self::GoImage => "lowering_go_projection_image",
            Self::GoIndexCapacity => "lowering_go_projection_index_capacity",
            Self::GoDepth => "lowering_go_projection_depth",
            Self::GoVariadicWithoutParameter => "lowering_go_projection_variadic_without_parameter",
            Self::GoAnchor => "lowering_go_projection_anchor",
            Self::GoListCapacity => "lowering_go_projection_list_capacity",
            Self::GoOrphanTarget => "lowering_go_projection_orphan_target",
            Self::GoForeignKey => "lowering_go_projection_foreign_key",
            Self::GoPackageLineage => "lowering_go_projection_package_lineage",
            Self::GoAtomUtf8 => "lowering_go_projection_atom_utf8",
            Self::GoRelativeSpan => "lowering_go_projection_relative_span",
            Self::GoOrphanOwner => "lowering_go_projection_orphan_owner",
            Self::GoAdmission => "lowering_go_projection_admission",
            Self::PythonForeignSpellingUtf8 => "lowering_python_projection_foreign_spelling_utf8",
            Self::PythonForeignKey => "lowering_python_projection_foreign_key",
            Self::PythonPackageLineage => "lowering_python_projection_package_lineage",
            Self::PythonContainment => "lowering_python_projection_containment",
            Self::CSharpImage => "lowering_csharp_projection_image",
            Self::CSharpDepth => "lowering_csharp_projection_depth",
            Self::CSharpNameSpan => "lowering_csharp_projection_name_span",
            Self::CSharpOwnerOrder => "lowering_csharp_projection_owner_order",
            Self::CSharpAnchor => "lowering_csharp_projection_anchor",
            Self::CSharpForeign => "lowering_csharp_projection_foreign",
            Self::CSharpAttributeCapacity => "lowering_csharp_projection_attribute_capacity",
            Self::CSharpIndexCapacity => "lowering_csharp_projection_index_capacity",
            Self::CSharpHeterogeneousArrayRank => {
                "lowering_csharp_projection_heterogeneous_array_rank"
            }
            Self::CSharpResultName => "lowering_csharp_projection_result_name",
            Self::CSharpAdmission => "lowering_csharp_projection_admission",
            Self::ClangSpan => "lowering_clang_projection_span",
            Self::ClangNameless => "lowering_clang_projection_nameless",
            Self::ClangAnchor => "lowering_clang_projection_anchor",
            Self::ClangIndexCapacity => "lowering_clang_projection_index_capacity",
            Self::ClangForeignOverride => "lowering_clang_projection_foreign_override",
            Self::ClangForeignReference => "lowering_clang_projection_foreign_reference",
            Self::ClangIllegalQualifierTarget => {
                "lowering_clang_projection_illegal_qualifier_target"
            }
            Self::ClangIllegalMemberPointerOwner => {
                "lowering_clang_projection_illegal_member_pointer_owner"
            }
        }
    }
}

/// Exact portable constructor-admission fault.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "fault", rename_all = "snake_case", deny_unknown_fields)]
pub enum PackageProjectionConstructorFaultFacts {
    /// The constructor tag is outside the closed constructor vocabulary.
    Tag {
        /// Numeric constructor tag found in the rejected record.
        actual: u32,
    },
    /// A constructor uses payload cells reserved for its tag.
    ReservedPayload {
        /// Compact numeric tag identifying the constructor.
        tag: u8,
        /// First constructor payload cell; its meaning or reserved status depends on the closed tag.
        payload0: u32,
        /// Second payload cell; function tags use it for result count, while other tags may reserve it.
        payload1: u32,
    },
    /// The constructor child arity overflows its encoded payload.
    ArityOverflow {
        /// Compact numeric tag identifying the constructor.
        tag: u8,
        /// Function parameter count before checked addition with the result count.
        payload0: u32,
        /// Function result count before checked addition with the parameter count.
        payload1: u32,
    },
    /// The constructor has a child count different from the required arity.
    Arity {
        /// Compact numeric tag identifying the constructor.
        tag: u8,
        /// Value required by the closed record shape.
        expected: u32,
        /// Number of admitted child facts supplied for the constructor.
        actual: u32,
    },
}

impl PackageProjectionConstructorFaultFacts {
    const fn kind_tag(self) -> &'static str {
        match self {
            Self::Tag { .. } => "lowering_projection_constructor_tag",
            Self::ReservedPayload { .. } => "lowering_projection_constructor_reserved_payload",
            Self::ArityOverflow { .. } => "lowering_projection_constructor_arity_overflow",
            Self::Arity { .. } => "lowering_projection_constructor_arity",
        }
    }
}

/// Exact portable semantic-type fault.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "fault", rename_all = "snake_case", deny_unknown_fields)]
pub enum PackageProjectionSemanticTypeFaultFacts {
    /// The semantic type tag is outside the closed type vocabulary.
    Tag {
        /// Numeric semantic type tag found in the rejected record.
        actual: u8,
    },
    /// A cell reserved for this type tag contains a nonzero value.
    ReservedCell {
        /// Semantic type form whose reserved payload cell must be zero.
        tag: PackageTypeTagFact,
        /// Type-record cell that is missing or unexpectedly populated.
        cell: PackageTypeCellFact,
        /// Nonzero cell value; a forbidden present text cell is represented by one.
        actual: u32,
    },
    /// A required cell for this type tag is absent.
    MissingCell {
        /// Semantic type form that requires the absent cell.
        tag: PackageTypeTagFact,
        /// Required payload or text cell that was absent.
        cell: PackageTypeCellFact,
    },
    /// The type record contains an unknown reason code.
    Reason {
        /// Unknown reason code found in the type record.
        actual: u32,
    },
    /// The primitive type shape code is invalid.
    PrimitiveShape {
        /// Primitive shape code found in the type record.
        actual: u32,
    },
    /// The cv-qualifier bits are invalid.
    CvQualifiers {
        /// Qualifier bit field found in the type record.
        actual: u32,
    },
    /// The type record width is invalid.
    Width {
        /// Width value found in the type record.
        actual: u32,
    },
    /// The type tag has a child count outside its permitted bounds.
    ChildCount {
        /// Semantic type tag whose cell or child constraint failed.
        tag: PackageTypeTagFact,
        /// Minimum permitted child count for the type tag.
        min: u32,
        /// Maximum permitted child count for the type tag.
        max: u32,
        /// Number of children found on the type record.
        actual: u32,
    },
    /// This type tag forbids a name on the indicated child.
    ChildNameForbidden {
        /// Semantic type form that forbids a name on this child.
        tag: PackageTypeTagFact,
        /// Zero-based position of the child with the invalid property.
        position: u32,
    },
    /// This type tag requires a name on the indicated child.
    ChildNameRequired {
        /// Semantic type form that requires a name on this child.
        tag: PackageTypeTagFact,
        /// Zero-based position of the child with the invalid property.
        position: u32,
    },
    /// The indicated child has flags forbidden for this type tag.
    ChildFlagsForbidden {
        /// Semantic type form that determines the allowed child flags.
        tag: PackageTypeTagFact,
        /// Zero-based position of the child with the invalid property.
        position: u32,
        /// Child flags found at the indicated position.
        actual: u8,
    },
    /// The indicated child has an invalid variadic-parameter marker.
    VariadicParameter {
        /// Zero-based position of the child with the invalid property.
        position: u32,
        /// Variadic marker value observed on the child.
        actual: u8,
    },
    /// This type tag forbids text on the indicated child.
    ChildTextForbidden {
        /// Semantic type form that forbids text on this child.
        tag: PackageTypeTagFact,
        /// Zero-based position of the child with the invalid property.
        position: u32,
    },
}

impl PackageProjectionSemanticTypeFaultFacts {
    const fn kind_tag(self) -> &'static str {
        match self {
            Self::Tag { .. } => "lowering_projection_type_tag",
            Self::ReservedCell { .. } => "lowering_projection_reserved_cell",
            Self::MissingCell { .. } => "lowering_projection_missing_cell",
            Self::Reason { .. } => "lowering_projection_unknown_reason",
            Self::PrimitiveShape { .. } => "lowering_projection_primitive_shape",
            Self::CvQualifiers { .. } => "lowering_projection_cv_qualifiers",
            Self::Width { .. } => "lowering_projection_width",
            Self::ChildCount { .. } => "lowering_projection_child_count",
            Self::ChildNameForbidden { .. } => "lowering_projection_child_name_forbidden",
            Self::ChildNameRequired { .. } => "lowering_projection_child_name_required",
            Self::ChildFlagsForbidden { .. } => "lowering_projection_child_flags_forbidden",
            Self::VariadicParameter { .. } => "lowering_projection_variadic_parameter",
            Self::ChildTextForbidden { .. } => "lowering_projection_child_text_forbidden",
        }
    }
}

/// Closed semantic type-tag operand.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageTypeTagFact {
    /// The semantic type tag for the receiver type.
    SelfType,
    /// The semantic type tag for a primitive type.
    Primitive,
    /// The semantic type tag for a tuple type.
    Tuple,
    /// The semantic type tag for a slice type.
    Slice,
    /// The semantic type tag for an array type.
    Array,
    /// The semantic type tag for a union type.
    Union,
    /// The semantic type tag for an intersection type.
    Intersection,
    /// The semantic type tag for the never type.
    Never,
    /// The semantic type tag for the any type.
    Any,
    /// The semantic type tag for an unknown type.
    Unknown,
    /// The semantic type tag for a nominal type.
    Nominal,
    /// The semantic type tag for a type application.
    Apply,
    /// The semantic type tag for a type variable.
    TypeVar,
    /// The semantic type tag for a wildcard type.
    Wildcard,
    /// The semantic type tag for a function-pointer type.
    FunctionPointer,
    /// The semantic type tag for an annotated type.
    Annotated,
    /// The semantic type tag for a conditional type.
    Conditional,
    /// The semantic type tag for a mapped type.
    Mapped,
    /// The semantic type tag for a template-literal type.
    TemplateLiteral,
    /// The semantic type tag for an anonymous record type.
    AnonymousRecord,
    /// The semantic type tag for an impl-trait type.
    ImplTrait,
    /// The semantic type tag for a dyn-trait type.
    DynTrait,
    /// The semantic type tag for an inferred type.
    Inferred,
    /// The semantic type tag for a qualified path type.
    QualifiedPath,
    /// The semantic type tag for a map type.
    Map,
    /// The semantic type tag for a channel type.
    Channel,
    /// The semantic type tag for an array-sequence type.
    ArraySequence,
    /// The semantic type tag for a rectangular-array type.
    ArrayRectangular,
    /// The semantic type tag for a fixed-array type.
    ArrayFixed,
    /// The semantic type tag for an array type with a constant-expression bound.
    ArrayConstExpression,
    /// The semantic type tag for an incomplete-array type.
    ArrayIncomplete,
    /// The semantic type tag for a C-qualified type.
    CQualified,
    /// The semantic type tag for a keyof type.
    KeyOf,
    /// The semantic type tag for an indexed-access type.
    IndexedAccess,
    /// The semantic type tag for a typeof type.
    TypeOf,
}

/// Closed type-record cell operand.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageTypeCellFact {
    /// The first numeric payload cell.
    Payload0,
    /// The second numeric payload cell.
    Payload1,
    /// The primary text cell.
    Text,
    /// The secondary text cell.
    Text2,
    /// The nominal-identity cell.
    Nominal,
}

/// Exact portable projection-admission fault. Each variant requires only its
/// own operands; no optional numeric coordinate bag is admitted.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "fault", rename_all = "snake_case", deny_unknown_fields)]
pub enum PackageProjectionAdmissionFaultFacts {
    /// A projected declaration name is empty.
    EmptyName,
    /// An anonymous callable could not be assigned a stable source anchor.
    AnonymousCallableAnchor {
        /// Closed cause carrying the details for this fault.
        cause: PackageAnonymousCallableAnchorFaultFacts,
    },
    /// The anonymous-callable anchor pool exceeded its bounded capacity.
    AnonymousCallableAnchorPoolCapacity {
        /// Bounded anonymous-callable anchor pool that reached its limit.
        pool: PackageAnonymousCallableAnchorPoolFact,
        /// Existing retained entries or bytes in the selected pool.
        used: u64,
        /// Number of additional entries or bytes required from the selected pool.
        requested: u64,
        /// Maximum entries or bytes allowed in the selected pool.
        capacity: u64,
    },
    /// A bounded projection table has no remaining capacity.
    Capacity,
    /// The projected child table has no remaining capacity.
    ChildCapacity,
    /// The product-type child pool exceeded its bounded capacity.
    ProductChildPoolCapacity {
        /// Product-child entries already retained in the flat arena.
        used: u64,
        /// Additional product-type children requested by the projection.
        requested: u64,
        /// Maximum number of product-type child entries allowed in the pool.
        capacity: u64,
    },
    /// A portable constructor record failed validation.
    Constructor {
        /// Closed cause carrying the details for this fault.
        cause: PackageProjectionConstructorFaultFacts,
    },
    /// A child has a role different from the role required at its position.
    ChildRole {
        /// Zero-based position of the child with the invalid property.
        position: u64,
        /// Child-role value required at this position.
        expected: u8,
        /// Child-role value present at this position.
        actual: u8,
    },
    /// A child target index does not identify a projected fact.
    ChildTarget {
        /// Zero-based product-child position containing the target.
        position: u64,
        /// Zero-based fact ordinal named by this product child.
        target: u32,
        /// Number of facts already pushed; valid target ordinals are smaller.
        fact_count: u64,
    },
    /// A semantic type record failed validation.
    TypeRecord {
        /// Closed cause carrying the details for this fault.
        cause: PackageProjectionSemanticTypeFaultFacts,
    },
    /// A semantic type child failed validation.
    TypeChild {
        /// Zero-based semantic-type child position that violated its child law.
        position: u64,
        /// Closed cause carrying the details for this fault.
        cause: PackageProjectionSemanticTypeFaultFacts,
    },
    /// A semantic type child target does not identify a projected fact.
    TypeChildTarget {
        /// Zero-based semantic-type child position containing the target.
        position: u64,
        /// Zero-based fact ordinal named by this type child.
        target: u32,
        /// Number of facts already pushed; valid target ordinals are smaller.
        fact_count: u64,
    },
    /// The semantic type child table has no remaining capacity.
    TypeChildCapacity,
    /// A semantic type child lane exceeded its bounded capacity.
    TypeChildPoolCapacity {
        /// Closed type-child arena lane whose bound was reached.
        lane: u8,
        /// Number of child entries already retained in this lane.
        used: u64,
        /// Additional child entries required by the pending row.
        requested: u64,
        /// Maximum number of type-child entries allowed in this lane.
        capacity: u64,
    },
    /// The semantic type row table has no remaining capacity.
    TypeRowCapacity,
    /// The computed-row table has no remaining capacity.
    ComputedRowCapacity,
    /// Recursive type projection exceeded its maximum depth.
    TypeProjectionDepthLimit {
        /// Depth observed at the rejected type node.
        depth: u64,
        /// Maximum depth allowed for recursive type projection.
        maximum: u64,
    },
    /// Type projection encountered a cycle at the reported type id.
    TypeProjectionCycle {
        /// Interned type handle that closes the cycle.
        type_id: u32,
        /// Exact declaration span that owned this projection, when retained
        /// by the language lowerer. This is not the recursive type-use edge.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        owner_span: Option<backend_semantic::vocabulary::ProjectionSpan>,
        /// Exact identifier span of the owning declaration, when retained.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        owner_name_span: Option<backend_semantic::vocabulary::ProjectionSpan>,
    },
    /// A recursive type reference exceeds the supported reference distance.
    TypeProjectionRecursiveReference {
        /// Number of enclosing recursive scopes to reference.
        distance: u32,
    },
    /// Type projection produced more children than its maximum width.
    TypeProjectionWidth {
        /// Exact number of children required by the source constructor.
        actual: u64,
        /// Maximum number of children allowed in the type projection.
        maximum: u64,
    },
    /// An occurrence owner index does not identify a projected fact.
    OccurrenceOwner {
        /// Zero-based emitted fact ordinal claimed as the occurrence owner.
        owner: u32,
        /// Number of facts already pushed; valid owner ordinals are smaller.
        fact_count: u64,
    },
    /// The occurrence table has no remaining capacity.
    OccurrenceCapacity,
    /// A documentation owner index does not identify a projected fact.
    DocOwner {
        /// Zero-based emitted fact ordinal claimed as the documentation owner.
        owner: u32,
        /// Number of facts already pushed; valid owner ordinals are smaller.
        fact_count: u64,
    },
    /// The documentation table has no remaining capacity.
    DocCapacity,
    /// The extension-atom table has no remaining capacity.
    ExtensionAtomCapacity,
    /// The type-parameter table has no remaining capacity.
    TypeParameterCapacity,
    /// The requested type-parameter bounds exceed available capacity.
    TypeParameterBoundCapacity {
        /// Cumulative bound-plane endpoint after append; host `usize::MAX` marks checked-add overflow.
        requested: u64,
        /// Total planned capacity of the ordered type/lifetime bound lane.
        available: u64,
    },
    /// The reference-list table has no remaining capacity.
    RefListCapacity,
    /// A reference list contains an unsupported element.
    RefListElements,
    /// A reference target index does not identify a projected fact.
    RefTarget {
        /// Closed relation lane in which the pooled target was read.
        lane: u8,
        /// Raw zero-based fact ordinal stored as the relation target.
        raw: u32,
        /// Number of facts already pushed; valid target ordinals are smaller.
        fact_count: u64,
    },
    /// A source span is outside the selected source bounds.
    SourceSpan {
        /// Zero-based emitted entity row whose span escaped the selected source.
        entity: u32,
        /// Inclusive source-byte offset of the reported entity span.
        start: u32,
        /// Exclusive source-byte offset of the reported entity span.
        end: u32,
        /// Byte length of the selected source.
        source_len: u32,
    },
    /// Two projections assign different source spans to the same entity.
    ConflictingSourceSpan {
        /// Entity assigned different spans by the two projections.
        entity: u32,
        /// Inclusive source-byte offset in the previously admitted span.
        existing_start: u32,
        /// Exclusive source-byte offset in the previously admitted span.
        existing_end: u32,
        /// Inclusive source-byte offset in the newly requested span.
        requested_start: u32,
        /// Exclusive source-byte offset in the newly requested span.
        requested_end: u32,
    },
    /// Two projections assign different parentage to the same entity.
    ConflictingParentage {
        /// Entity assigned different parentage by the two projections.
        entity: u32,
        /// Parentage state in the first projection.
        existing: PackageParentageFact,
        /// Parentage state requested by the later projection.
        requested: PackageParentageFact,
    },
    /// Two projections give different member inventories for the same entity.
    ConflictingMemberInventory {
        /// Entity assigned different member inventories by the two projections.
        entity: u32,
        /// Number of members in the previously admitted inventory.
        existing_count: u64,
        /// Number of members in the newly requested inventory.
        requested_count: u64,
        /// First member position where the inventories differ.
        first_difference: u64,
        /// Member index at the first difference in the existing inventory, if present.
        existing_member: Option<u32>,
        /// Member index at the first difference in the requested inventory, if present.
        requested_member: Option<u32>,
    },
}

impl PackageProjectionAdmissionFaultFacts {
    const fn kind_tag(self) -> &'static str {
        match self {
            Self::EmptyName => "lowering_projection_empty_name",
            Self::AnonymousCallableAnchor { .. } => "lowering_projection_anonymous_callable_anchor",
            Self::AnonymousCallableAnchorPoolCapacity { pool, .. } => match pool {
                PackageAnonymousCallableAnchorPoolFact::Entries => {
                    "lowering_projection_anonymous_callable_anchor_entry_capacity"
                }
                PackageAnonymousCallableAnchorPoolFact::Bytes => {
                    "lowering_projection_anonymous_callable_anchor_byte_capacity"
                }
            },
            Self::Capacity => "lowering_projection_capacity",
            Self::ChildCapacity => "lowering_projection_child_capacity",
            Self::ProductChildPoolCapacity { .. } => {
                "lowering_projection_product_child_pool_capacity"
            }
            Self::Constructor { cause } => cause.kind_tag(),
            Self::ChildRole { .. } => "lowering_projection_child_role",
            Self::ChildTarget { .. } => "lowering_projection_child_target",
            Self::TypeRecord { cause } => match cause {
                PackageProjectionSemanticTypeFaultFacts::MissingCell { .. } => {
                    "lowering_projection_type_record_missing_cell"
                }
                _ => cause.kind_tag(),
            },
            Self::TypeChild { cause, .. } => match cause {
                PackageProjectionSemanticTypeFaultFacts::ChildNameRequired { .. } => {
                    "lowering_projection_type_child_child_name_required"
                }
                PackageProjectionSemanticTypeFaultFacts::MissingCell { .. } => {
                    "lowering_projection_type_child_missing_cell"
                }
                _ => cause.kind_tag(),
            },
            Self::TypeChildTarget { .. } => "lowering_projection_type_child_target",
            Self::TypeChildCapacity => "lowering_projection_type_child_capacity",
            Self::TypeChildPoolCapacity { .. } => "lowering_projection_type_child_pool_capacity",
            Self::TypeRowCapacity => "lowering_projection_type_row_capacity",
            Self::ComputedRowCapacity => "lowering_projection_computed_row_capacity",
            Self::TypeProjectionDepthLimit { .. } => {
                "lowering_projection_type_projection_depth_limit"
            }
            Self::TypeProjectionCycle { .. } => "lowering_projection_type_projection_cycle",
            Self::TypeProjectionRecursiveReference { .. } => {
                "lowering_projection_type_projection_recursive_reference"
            }
            Self::TypeProjectionWidth { .. } => "lowering_projection_type_projection_width",
            Self::OccurrenceOwner { .. } => "lowering_projection_occurrence_owner",
            Self::OccurrenceCapacity => "lowering_projection_occurrence_capacity",
            Self::DocOwner { .. } => "lowering_projection_doc_owner",
            Self::DocCapacity => "lowering_projection_doc_capacity",
            Self::ExtensionAtomCapacity => "lowering_projection_extension_atom_capacity",
            Self::TypeParameterCapacity => "lowering_projection_type_parameter_capacity",
            Self::TypeParameterBoundCapacity { .. } => {
                "lowering_projection_type_parameter_bound_capacity"
            }
            Self::RefListCapacity => "lowering_projection_ref_list_capacity",
            Self::RefListElements => "lowering_projection_ref_list_elements",
            Self::RefTarget { .. } => "lowering_projection_ref_target",
            Self::SourceSpan { .. } => "lowering_projection_source_span",
            Self::ConflictingSourceSpan { .. } => "lowering_projection_conflicting_source_span",
            Self::ConflictingMemberInventory { .. } => {
                "lowering_projection_conflicting_member_inventory"
            }
            Self::ConflictingParentage { .. } => "lowering_projection_conflicting_parentage",
        }
    }
}

/// Closed reason an anonymous callable anchor could not be admitted.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageAnonymousCallableAnchorFaultFacts {
    InvalidRoute,
    Encoding,
    SourceIdentityUnavailable,
}

/// Bounded anonymous callable anchor pool that reached its limit.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageAnonymousCallableAnchorPoolFact {
    Entries,
    Bytes,
}

/// Closed parentage fact from conflicting authority projections.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum PackageParentageFact {
    /// The source authority did not provide parentage.
    Unavailable,
    /// The entity is a root with no parent.
    Root,
    /// The entity is bound to a represented parent.
    Bound {
        /// Index of the represented parent fact.
        parent: u32,
    },
    /// The parent is an authority owner absent from the projected facts.
    UnrepresentedAuthorityOwner {
        /// Opaque stable identity of the authority owner omitted from projected facts.
        identity: [u8; 16],
    },
}

/// Closed foreign-key grammar fault.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageForeignKeyFaultFacts {
    /// The foreign-key path is empty.
    EmptyPath,
    /// The foreign-key path contains a backslash.
    BackslashInPath,
}

/// Closed package-lineage grammar fault.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageLineageFaultFacts {
    /// The ecosystem component is empty.
    EmptyEcosystem,
    /// The package component is empty.
    EmptyPackage,
    /// The ecosystem component contains a package separator.
    SeparatorInEcosystem,
    /// The package component contains a lineage separator.
    SeparatorInPackage,
    /// The ecosystem component contains a backslash.
    BackslashEcosystem,
    /// The package component contains a backslash.
    BackslashPackage,
    /// A lineage component contains a backslash at the reported segment.
    BackslashInvalid {
        /// Zero-based path component whose spelling is non-canonical.
        segment: u8,
    },
}

// The source enums and all conversion matches are kept together so new
// semantic-vocabulary variants fail compilation until this projection is
// reviewed.
impl From<NativeTool> for CompilerNativeToolFact {
    fn from(value: NativeTool) -> Self {
        match value {
            NativeTool::Rustc => Self::Rustc,
            NativeTool::Clang => Self::Clang,
            NativeTool::Python => Self::Python,
            NativeTool::TypeScriptCompiler => Self::TypeScriptCompiler,
            NativeTool::GoCompiler => Self::GoCompiler,
            NativeTool::JavaCompiler => Self::JavaCompiler,
            NativeTool::CSharpCompiler => Self::CSharpCompiler,
        }
    }
}

impl From<AuthorityPhase> for AuthorityPhaseFact {
    fn from(value: AuthorityPhase) -> Self {
        match value {
            AuthorityPhase::Open => Self::Open,
            AuthorityPhase::Parse => Self::Parse,
            AuthorityPhase::Resolve => Self::Resolve,
            AuthorityPhase::TypeCheck => Self::TypeCheck,
            AuthorityPhase::Project => Self::Project,
        }
    }
}

impl From<AuthorityDiagnosticClass> for AuthorityClassFact {
    fn from(value: AuthorityDiagnosticClass) -> Self {
        match value {
            AuthorityDiagnosticClass::Syntax => Self::Syntax,
            AuthorityDiagnosticClass::Binding => Self::Binding,
            AuthorityDiagnosticClass::SourceScope => Self::SourceScope,
            AuthorityDiagnosticClass::Type => Self::Type,
            AuthorityDiagnosticClass::Authority => Self::Authority,
            AuthorityDiagnosticClass::Projection => Self::Projection,
        }
    }
}

impl From<backend_semantic::vocabulary::Language> for CompilerLanguageFact {
    fn from(value: backend_semantic::vocabulary::Language) -> Self {
        use backend_semantic::vocabulary::Language as L;
        match value {
            L::Rust => Self::Rust,
            L::TypeScript => Self::TypeScript,
            L::Python => Self::Python,
            L::Go => Self::Go,
            L::Java => Self::Java,
            L::CSharp => Self::CSharp,
            L::Clang => Self::Clang,
        }
    }
}

impl From<backend_semantic::vocabulary::Stage> for CompilerStageFact {
    fn from(value: backend_semantic::vocabulary::Stage) -> Self {
        use backend_semantic::vocabulary::Stage as S;
        match value {
            S::Parse => Self::Parse,
            S::LowerIr => Self::LowerIr,
        }
    }
}

fn constructor_tag(value: ProjectionConstructorTag) -> u8 {
    match value {
        ProjectionConstructorTag::Function => 0,
        ProjectionConstructorTag::Generic => 1,
        ProjectionConstructorTag::Tuple => 2,
        ProjectionConstructorTag::Array => 3,
        ProjectionConstructorTag::Union => 4,
        ProjectionConstructorTag::Intersection => 5,
        ProjectionConstructorTag::Product => 6,
    }
}

impl From<ProjectionConstructorFault> for PackageProjectionConstructorFaultFacts {
    fn from(value: ProjectionConstructorFault) -> Self {
        match value {
            ProjectionConstructorFault::Tag { actual } => Self::Tag { actual },
            ProjectionConstructorFault::ReservedPayload {
                tag,
                payload0,
                payload1,
            } => Self::ReservedPayload {
                tag: constructor_tag(tag),
                payload0,
                payload1,
            },
            ProjectionConstructorFault::ArityOverflow {
                tag,
                payload0,
                payload1,
            } => Self::ArityOverflow {
                tag: constructor_tag(tag),
                payload0,
                payload1,
            },
            ProjectionConstructorFault::Arity {
                tag,
                expected,
                actual,
            } => Self::Arity {
                tag: constructor_tag(tag),
                expected,
                actual,
            },
        }
    }
}

impl From<ProjectionSemanticTypeTag> for PackageTypeTagFact {
    fn from(value: ProjectionSemanticTypeTag) -> Self {
        use ProjectionSemanticTypeTag as T;
        match value {
            T::SelfType => Self::SelfType,
            T::Primitive => Self::Primitive,
            T::Tuple => Self::Tuple,
            T::Slice => Self::Slice,
            T::Array => Self::Array,
            T::Union => Self::Union,
            T::Intersection => Self::Intersection,
            T::Never => Self::Never,
            T::Any => Self::Any,
            T::Unknown => Self::Unknown,
            T::Nominal => Self::Nominal,
            T::Apply => Self::Apply,
            T::TypeVar => Self::TypeVar,
            T::Wildcard => Self::Wildcard,
            T::FunctionPointer => Self::FunctionPointer,
            T::Annotated => Self::Annotated,
            T::Conditional => Self::Conditional,
            T::Mapped => Self::Mapped,
            T::TemplateLiteral => Self::TemplateLiteral,
            T::AnonymousRecord => Self::AnonymousRecord,
            T::ImplTrait => Self::ImplTrait,
            T::DynTrait => Self::DynTrait,
            T::Inferred => Self::Inferred,
            T::QualifiedPath => Self::QualifiedPath,
            T::Map => Self::Map,
            T::Channel => Self::Channel,
            T::ArraySequence => Self::ArraySequence,
            T::ArrayRectangular => Self::ArrayRectangular,
            T::ArrayFixed => Self::ArrayFixed,
            T::ArrayConstExpression => Self::ArrayConstExpression,
            T::ArrayIncomplete => Self::ArrayIncomplete,
            T::CQualified => Self::CQualified,
            T::KeyOf => Self::KeyOf,
            T::IndexedAccess => Self::IndexedAccess,
            T::TypeOf => Self::TypeOf,
        }
    }
}

impl From<ProjectionTypeCell> for PackageTypeCellFact {
    fn from(value: ProjectionTypeCell) -> Self {
        match value {
            ProjectionTypeCell::Payload0 => Self::Payload0,
            ProjectionTypeCell::Payload1 => Self::Payload1,
            ProjectionTypeCell::Text => Self::Text,
            ProjectionTypeCell::Text2 => Self::Text2,
            ProjectionTypeCell::Nominal => Self::Nominal,
        }
    }
}

impl From<ProjectionSemanticTypeFault> for PackageProjectionSemanticTypeFaultFacts {
    fn from(value: ProjectionSemanticTypeFault) -> Self {
        use ProjectionSemanticTypeFault as F;
        match value {
            F::Tag { actual } => Self::Tag { actual },
            F::ReservedCell { tag, cell, actual } => Self::ReservedCell {
                tag: tag.into(),
                cell: cell.into(),
                actual,
            },
            F::MissingCell { tag, cell } => Self::MissingCell {
                tag: tag.into(),
                cell: cell.into(),
            },
            F::Reason { actual } => Self::Reason { actual },
            F::PrimitiveShape { actual } => Self::PrimitiveShape { actual },
            F::CvQualifiers { actual } => Self::CvQualifiers { actual },
            F::Width { actual } => Self::Width { actual },
            F::ChildCount {
                tag,
                min,
                max,
                actual,
            } => Self::ChildCount {
                tag: tag.into(),
                min,
                max,
                actual,
            },
            F::ChildNameForbidden { tag, position } => Self::ChildNameForbidden {
                tag: tag.into(),
                position,
            },
            F::ChildNameRequired { tag, position } => Self::ChildNameRequired {
                tag: tag.into(),
                position,
            },
            F::ChildFlagsForbidden {
                tag,
                position,
                actual,
            } => Self::ChildFlagsForbidden {
                tag: tag.into(),
                position,
                actual,
            },
            F::VariadicParameter { position, actual } => {
                Self::VariadicParameter { position, actual }
            }
            F::ChildTextForbidden { tag, position } => Self::ChildTextForbidden {
                tag: tag.into(),
                position,
            },
        }
    }
}

impl From<ProjectionAdmissionFault> for PackageProjectionAdmissionFaultFacts {
    fn from(value: ProjectionAdmissionFault) -> Self {
        use ProjectionAdmissionFault as F;
        match value {
            F::EmptyName => Self::EmptyName,
            F::AnonymousCallableAnchor { cause } => Self::AnonymousCallableAnchor {
                cause: match cause {
                    ProjectionAnonymousCallableAnchorFault::InvalidRoute => {
                        PackageAnonymousCallableAnchorFaultFacts::InvalidRoute
                    }
                    ProjectionAnonymousCallableAnchorFault::Encoding => {
                        PackageAnonymousCallableAnchorFaultFacts::Encoding
                    }
                    ProjectionAnonymousCallableAnchorFault::SourceIdentityUnavailable => {
                        PackageAnonymousCallableAnchorFaultFacts::SourceIdentityUnavailable
                    }
                },
            },
            F::AnonymousCallableAnchorPoolCapacity {
                pool,
                used,
                requested,
                capacity,
            } => Self::AnonymousCallableAnchorPoolCapacity {
                pool: match pool {
                    ProjectionAnonymousCallableAnchorPool::Entries => {
                        PackageAnonymousCallableAnchorPoolFact::Entries
                    }
                    ProjectionAnonymousCallableAnchorPool::Bytes => {
                        PackageAnonymousCallableAnchorPoolFact::Bytes
                    }
                },
                used,
                requested,
                capacity,
            },
            F::Capacity => Self::Capacity,
            F::ChildCapacity => Self::ChildCapacity,
            F::ProductChildPoolCapacity {
                used,
                requested,
                capacity,
            } => Self::ProductChildPoolCapacity {
                used,
                requested,
                capacity,
            },
            F::Constructor { cause } => Self::Constructor {
                cause: cause.into(),
            },
            F::ChildRole {
                position,
                expected,
                actual,
            } => Self::ChildRole {
                position,
                expected: expected as u8,
                actual: actual as u8,
            },
            F::ChildTarget {
                position,
                target,
                fact_count,
            } => Self::ChildTarget {
                position,
                target,
                fact_count,
            },
            F::TypeRecord { cause } => Self::TypeRecord {
                cause: cause.into(),
            },
            F::TypeChild { position, cause } => Self::TypeChild {
                position,
                cause: cause.into(),
            },
            F::TypeChildTarget {
                position,
                target,
                fact_count,
            } => Self::TypeChildTarget {
                position,
                target,
                fact_count,
            },
            F::TypeChildCapacity => Self::TypeChildCapacity,
            F::TypeChildPoolCapacity {
                lane,
                used,
                requested,
                capacity,
            } => Self::TypeChildPoolCapacity {
                lane: lane as u8,
                used,
                requested,
                capacity,
            },
            F::TypeRowCapacity => Self::TypeRowCapacity,
            F::ComputedRowCapacity => Self::ComputedRowCapacity,
            F::TypeProjectionDepthLimit { depth, maximum } => {
                Self::TypeProjectionDepthLimit { depth, maximum }
            }
            F::TypeProjectionCycle {
                type_id,
                owner_span,
                owner_name_span,
            } => Self::TypeProjectionCycle {
                type_id,
                owner_span,
                owner_name_span,
            },
            F::TypeProjectionRecursiveReference { distance } => {
                Self::TypeProjectionRecursiveReference { distance }
            }
            F::TypeProjectionWidth { actual, maximum } => {
                Self::TypeProjectionWidth { actual, maximum }
            }
            F::OccurrenceOwner { owner, fact_count } => Self::OccurrenceOwner { owner, fact_count },
            F::OccurrenceCapacity => Self::OccurrenceCapacity,
            F::DocOwner { owner, fact_count } => Self::DocOwner { owner, fact_count },
            F::DocCapacity => Self::DocCapacity,
            F::ExtensionAtomCapacity => Self::ExtensionAtomCapacity,
            F::TypeParameterCapacity => Self::TypeParameterCapacity,
            F::TypeParameterBoundCapacity {
                requested,
                available,
            } => Self::TypeParameterBoundCapacity {
                requested,
                available,
            },
            F::RefListCapacity => Self::RefListCapacity,
            F::RefListElements => Self::RefListElements,
            F::RefTarget {
                lane,
                raw,
                fact_count,
            } => Self::RefTarget {
                lane: lane as u8,
                raw,
                fact_count,
            },
            F::SourceSpan {
                entity,
                start,
                end,
                source_len,
            } => Self::SourceSpan {
                entity,
                start,
                end,
                source_len,
            },
            F::ConflictingSourceSpan {
                entity,
                existing,
                requested,
            } => Self::ConflictingSourceSpan {
                entity,
                existing_start: existing.start,
                existing_end: existing.end,
                requested_start: requested.start,
                requested_end: requested.end,
            },
            F::ConflictingMemberInventory {
                entity,
                existing_count,
                requested_count,
                first_difference,
                existing_member,
                requested_member,
            } => Self::ConflictingMemberInventory {
                entity,
                existing_count,
                requested_count,
                first_difference,
                existing_member,
                requested_member,
            },
            F::ConflictingParentage {
                entity,
                existing,
                requested,
            } => Self::ConflictingParentage {
                entity,
                existing: parentage(existing),
                requested: parentage(requested),
            },
        }
    }
}

fn parentage(
    value: backend_semantic::vocabulary::ProjectionParentageState,
) -> PackageParentageFact {
    use backend_semantic::vocabulary::ProjectionParentageState as P;
    match value {
        P::Unavailable => PackageParentageFact::Unavailable,
        P::Root => PackageParentageFact::Root,
        P::Bound { parent } => PackageParentageFact::Bound { parent },
        P::UnrepresentedAuthorityOwner { identity } => {
            PackageParentageFact::UnrepresentedAuthorityOwner { identity }
        }
    }
}

impl From<LoweringUnsupported> for PackageLoweringFaultFacts {
    fn from(value: LoweringUnsupported) -> Self {
        use LoweringUnsupported as F;
        match value {
            F::NoSupportedDeclaration => Self::NoSupportedDeclaration,
            F::ExtensionAtomUnbound {
                row,
                provisional,
                atom_count,
            } => Self::ExtensionAtomUnbound {
                row,
                provisional,
                atom_count,
            },
            F::ExtensionTypeParametersUnbound {
                row,
                start,
                length,
                element_count,
            } => Self::ExtensionTypeParametersUnbound {
                row,
                start,
                length,
                element_count,
            },
            F::FactRejected {
                fact,
                name_len,
                cause,
            } => Self::FactRejected {
                fact,
                name_len,
                cause: cause.into(),
            },
            F::RustFunction => Self::RustFunction,
            F::RustConstantType => Self::RustConstantType,
            F::RustGenericParameter => Self::RustGenericParameter,
            F::PythonAssignmentName => Self::PythonAssignmentName,
            F::PythonAssignmentValue => Self::PythonAssignmentValue,
            F::ClangDeclarationForm => Self::ClangDeclarationForm,
            F::TypeScriptDeclarationForm => Self::TypeScriptDeclarationForm,
            F::TypeScriptDeclarationType => Self::TypeScriptDeclarationType,
            F::CSharpDeclarationForm => Self::CSharpDeclarationForm,
            F::CSharpDeclarationType => Self::CSharpDeclarationType,
            F::GoDeclarationForm => Self::GoDeclarationForm,
            F::GoDeclarationType => Self::GoDeclarationType,
            F::JavaDeclarationForm => Self::JavaDeclarationForm,
            F::JavaProjection { fault } => Self::JavaProjection {
                fault: java_projection_kind(fault),
            },
            F::GoProjection { fault } => Self::GoProjection {
                fault: go_projection_kind(fault),
            },
            F::TypeScriptProjection { fault } => Self::TypeScriptProjection {
                fault: typescript_projection(fault),
            },
            F::PythonProjection { fault } => Self::PythonProjection {
                fault: python_projection_kind(fault),
            },
            F::CSharpProjection { fault } => Self::CSharpProjection {
                fault: csharp_projection_kind(fault),
            },
            F::ClangProjection { fault } => Self::ClangProjection {
                fault: clang_projection_kind(fault),
            },
        }
    }
}

fn typescript_projection(
    value: TypeScriptProjectionFault,
) -> PackageTypeScriptProjectionFaultFacts {
    use TypeScriptProjectionFault as F;
    match value {
        F::ForeignKey { start, end, cause } => PackageTypeScriptProjectionFaultFacts::ForeignKey {
            start,
            end,
            cause: match cause {
                backend_semantic::vocabulary::ProjectionForeignKeyFault::EmptyPath => {
                    PackageForeignKeyFaultFacts::EmptyPath
                }
                backend_semantic::vocabulary::ProjectionForeignKeyFault::BackslashInPath => {
                    PackageForeignKeyFaultFacts::BackslashInPath
                }
            },
        },
        F::PackageLineage { start, end, cause } => {
            PackageTypeScriptProjectionFaultFacts::PackageLineage {
                start,
                end,
                cause: lineage(cause),
            }
        }
        F::CoordinateOverflow { value } => {
            PackageTypeScriptProjectionFaultFacts::CoordinateOverflow { value }
        }
        F::MissingImportBinding { fact } => {
            PackageTypeScriptProjectionFaultFacts::MissingImportBinding { fact }
        }
    }
}

fn lineage(
    value: backend_semantic::vocabulary::ProjectionPackageLineageFault,
) -> PackageLineageFaultFacts {
    use backend_semantic::vocabulary::{
        ProjectionLineagePart as P, ProjectionPackageLineageFault as F,
    };
    match value {
        F::EmptyEcosystem => PackageLineageFaultFacts::EmptyEcosystem,
        F::EmptyPackage => PackageLineageFaultFacts::EmptyPackage,
        F::SeparatorInEcosystem => PackageLineageFaultFacts::SeparatorInEcosystem,
        F::SeparatorInPackage => PackageLineageFaultFacts::SeparatorInPackage,
        F::Backslash { part: P::Ecosystem } => PackageLineageFaultFacts::BackslashEcosystem,
        F::Backslash { part: P::Package } => PackageLineageFaultFacts::BackslashPackage,
        F::Backslash {
            part: P::Invalid { segment },
        } => PackageLineageFaultFacts::BackslashInvalid { segment },
    }
}

fn java_projection_kind(value: JavaProjectionFault) -> PackageLanguageProjectionFault {
    use JavaProjectionFault as F;
    match value {
        F::Image { .. } => PackageLanguageProjectionFault::JavaImage,
        F::Depth { .. } => PackageLanguageProjectionFault::JavaDepth,
        F::MalformedType { .. } => PackageLanguageProjectionFault::JavaMalformedType,
        F::Primitive { .. } => PackageLanguageProjectionFault::JavaPrimitive,
        F::AtomUtf8 { .. } => PackageLanguageProjectionFault::JavaAtomUtf8,
        F::SourceUtf8 => PackageLanguageProjectionFault::JavaSourceUtf8,
        F::Utf16Offset { .. } => PackageLanguageProjectionFault::JavaUtf16Offset,
        F::Utf16Range { .. } => PackageLanguageProjectionFault::JavaUtf16Range,
        F::OrphanOwner { .. } => PackageLanguageProjectionFault::JavaOrphanOwner,
        F::ForeignKey { .. } => PackageLanguageProjectionFault::JavaForeignKey,
        F::SiblingCapacity { .. } => PackageLanguageProjectionFault::JavaSiblingCapacity,
        F::IndexCapacity { .. } => PackageLanguageProjectionFault::JavaIndexCapacity,
    }
}
fn go_projection_kind(value: GoProjectionFault) -> PackageLanguageProjectionFault {
    use GoProjectionFault as F;
    match value {
        F::Image { .. } => PackageLanguageProjectionFault::GoImage,
        F::IndexCapacity { .. } => PackageLanguageProjectionFault::GoIndexCapacity,
        F::Depth { .. } => PackageLanguageProjectionFault::GoDepth,
        F::VariadicWithoutParameter { .. } => {
            PackageLanguageProjectionFault::GoVariadicWithoutParameter
        }
        F::Anchor { .. } => PackageLanguageProjectionFault::GoAnchor,
        F::ListCapacity { .. } => PackageLanguageProjectionFault::GoListCapacity,
        F::OrphanTarget { .. } => PackageLanguageProjectionFault::GoOrphanTarget,
        F::ForeignKey { .. } => PackageLanguageProjectionFault::GoForeignKey,
        F::PackageLineage { .. } => PackageLanguageProjectionFault::GoPackageLineage,
        F::AtomUtf8 { .. } => PackageLanguageProjectionFault::GoAtomUtf8,
        F::RelativeSpan { .. } => PackageLanguageProjectionFault::GoRelativeSpan,
        F::OrphanOwner { .. } => PackageLanguageProjectionFault::GoOrphanOwner,
        F::Admission { .. } => PackageLanguageProjectionFault::GoAdmission,
    }
}
fn python_projection_kind(value: PythonProjectionFault) -> PackageLanguageProjectionFault {
    use PythonProjectionFault as F;
    match value {
        F::ForeignSpellingUtf8 { .. } => PackageLanguageProjectionFault::PythonForeignSpellingUtf8,
        F::ForeignKey { .. } => PackageLanguageProjectionFault::PythonForeignKey,
        F::PackageLineage { .. } => PackageLanguageProjectionFault::PythonPackageLineage,
        F::Containment { .. } => PackageLanguageProjectionFault::PythonContainment,
    }
}
fn csharp_projection_kind(
    value: backend_semantic::vocabulary::CSharpProjectionFault,
) -> PackageLanguageProjectionFault {
    use backend_semantic::vocabulary::CSharpProjectionFault as F;
    match value {
        F::Image { .. } => PackageLanguageProjectionFault::CSharpImage,
        F::Depth { .. } => PackageLanguageProjectionFault::CSharpDepth,
        F::NameSpan { .. } => PackageLanguageProjectionFault::CSharpNameSpan,
        F::OwnerOrder { .. } => PackageLanguageProjectionFault::CSharpOwnerOrder,
        F::Anchor { .. } => PackageLanguageProjectionFault::CSharpAnchor,
        F::Foreign { .. } => PackageLanguageProjectionFault::CSharpForeign,
        F::AttributeCapacity { .. } => PackageLanguageProjectionFault::CSharpAttributeCapacity,
        F::IndexCapacity { .. } => PackageLanguageProjectionFault::CSharpIndexCapacity,
        F::HeterogeneousArrayRank { .. } => {
            PackageLanguageProjectionFault::CSharpHeterogeneousArrayRank
        }
        F::ResultName { .. } => PackageLanguageProjectionFault::CSharpResultName,
        F::Admission { .. } => PackageLanguageProjectionFault::CSharpAdmission,
    }
}
fn clang_projection_kind(value: ClangProjectionFault) -> PackageLanguageProjectionFault {
    use ClangProjectionFault as F;
    match value {
        F::Span { .. } => PackageLanguageProjectionFault::ClangSpan,
        F::Nameless { .. } => PackageLanguageProjectionFault::ClangNameless,
        F::Anchor => PackageLanguageProjectionFault::ClangAnchor,
        F::IndexCapacity => PackageLanguageProjectionFault::ClangIndexCapacity,
        F::ForeignOverride { .. } => PackageLanguageProjectionFault::ClangForeignOverride,
        F::ForeignReference { .. } => PackageLanguageProjectionFault::ClangForeignReference,
        F::IllegalQualifierTarget { .. } => {
            PackageLanguageProjectionFault::ClangIllegalQualifierTarget
        }
        F::IllegalMemberPointerOwner { .. } => {
            PackageLanguageProjectionFault::ClangIllegalMemberPointerOwner
        }
    }
}

/// Projection of one compiler terminal into the package-failure family.
pub(crate) fn package_failure_from_terminal(
    terminal: &CompilerTerminal,
) -> Result<
    Option<(
        Option<ContentId<CompileRecipeDomain>>,
        PackageCompilerFailureCause,
    )>,
    super::ProductAdmissionError,
> {
    Ok(match terminal {
        CompilerTerminal::Toolchain {
            language,
            stage,
            selected,
            configured,
            ..
        } => Some((
            None,
            PackageCompilerFailureCause::Toolchain {
                language: (*language).into(),
                stage: (*stage).into(),
                selected: (*selected).into(),
                configured: configured.map(Into::into),
            },
        )),
        CompilerTerminal::ToolingUnavailable {
            language,
            stage,
            tool,
            ..
        } => Some((
            None,
            PackageCompilerFailureCause::ToolingUnavailable {
                language: (*language).into(),
                stage: (*stage).into(),
                tool: (*tool).into(),
            },
        )),
        CompilerTerminal::RequiredTool {
            language,
            stage,
            issue,
            ..
        } => Some((
            None,
            PackageCompilerFailureCause::RequiredTool {
                language: (*language).into(),
                stage: (*stage).into(),
                issue: *issue,
            },
        )),
        CompilerTerminal::AuthorityAdmission {
            language,
            stage,
            phase,
            class,
            diagnostic,
            ..
        } => Some((
            None,
            PackageCompilerFailureCause::AuthorityAdmission {
                language: (*language).into(),
                stage: (*stage).into(),
                phase: (*phase).into(),
                class: (*class).into(),
                diagnostic: diagnostic.as_ref().map(|diagnostic| {
                    CompilerAuthorityDiagnosticFacts {
                        retained_bytes: u32::try_from(diagnostic.byte_len).unwrap_or(u32::MAX),
                        observed_bytes: u64::try_from(diagnostic.observed).unwrap_or(u64::MAX),
                        truncated: diagnostic.truncated,
                        python_failure: diagnostic.python_failure,
                        python_configuration: diagnostic.python_configuration,
                        typescript_failure: diagnostic.typescript_failure,
                        typescript_stop_site: diagnostic.typescript_stop_site.clone(),
                        typescript_admission: diagnostic.typescript_admission.clone(),
                        typescript_cause: diagnostic.typescript_cause.clone(),
                        go_failure: diagnostic.go_failure,
                        rust_source_scope_failure: diagnostic.rust_source_scope_failure,
                    }
                }),
            },
        )),
        CompilerTerminal::Compile {
            attempted,
            cause: CompilerCause::FragmentFailure(failure),
        } => Some((
            Some(attempted.recipe),
            PackageCompilerFailureCause::Fragment(PackageCompilerFragmentFaultFacts::new(
                failure.kind(),
                failure.facts(),
            )?),
        )),
        CompilerTerminal::Compile {
            attempted,
            cause: CompilerCause::Lowering(cause),
        } => Some((
            Some(attempted.recipe),
            PackageCompilerFailureCause::Lowering((**cause).into()),
        )),
        CompilerTerminal::Compile {
            attempted,
            cause:
                CompilerCause::Authority {
                    phase,
                    class,
                    diagnostic,
                },
        } => Some((
            Some(attempted.recipe),
            PackageCompilerFailureCause::Authority {
                phase: (*phase).into(),
                class: (*class).into(),
                diagnostic: diagnostic.as_ref().map(|diagnostic| {
                    CompilerAuthorityDiagnosticFacts {
                        retained_bytes: u32::try_from(diagnostic.byte_len).unwrap_or(u32::MAX),
                        observed_bytes: u64::try_from(diagnostic.observed).unwrap_or(u64::MAX),
                        truncated: diagnostic.truncated,
                        python_failure: diagnostic.python_failure,
                        python_configuration: diagnostic.python_configuration,
                        typescript_failure: diagnostic.typescript_failure,
                        typescript_stop_site: diagnostic.typescript_stop_site.clone(),
                        typescript_admission: diagnostic.typescript_admission.clone(),
                        typescript_cause: diagnostic.typescript_cause.clone(),
                        go_failure: diagnostic.go_failure,
                        rust_source_scope_failure: diagnostic.rust_source_scope_failure,
                    }
                }),
            },
        )),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_match_host_registry_programs() {
        assert_eq!(
            CompilerNativeToolFact::TypeScriptCompiler.executable(),
            "tsc"
        );
        assert_eq!(
            CompilerNativeToolFact::TypeScriptCompiler.configuration_variable(),
            "NUDOX_TSC"
        );
        assert_eq!(
            CompilerNativeToolFact::TypeScriptCompiler.project_local_path(),
            Some("node_modules/.bin/tsc")
        );
    }

    #[test]
    fn typed_typescript_projection_faults_keep_closed_wire_tags_and_operands() {
        for (source, expected, tag, wire_fault) in [
            (
                ProjectionAdmissionFault::AnonymousCallableAnchor {
                    cause: ProjectionAnonymousCallableAnchorFault::SourceIdentityUnavailable,
                },
                PackageProjectionAdmissionFaultFacts::AnonymousCallableAnchor {
                    cause: PackageAnonymousCallableAnchorFaultFacts::SourceIdentityUnavailable,
                },
                "lowering_projection_anonymous_callable_anchor",
                "anonymous_callable_anchor",
            ),
            (
                ProjectionAdmissionFault::AnonymousCallableAnchorPoolCapacity {
                    pool: ProjectionAnonymousCallableAnchorPool::Entries,
                    used: 4,
                    requested: 1,
                    capacity: 4,
                },
                PackageProjectionAdmissionFaultFacts::AnonymousCallableAnchorPoolCapacity {
                    pool: PackageAnonymousCallableAnchorPoolFact::Entries,
                    used: 4,
                    requested: 1,
                    capacity: 4,
                },
                "lowering_projection_anonymous_callable_anchor_entry_capacity",
                "anonymous_callable_anchor_pool_capacity",
            ),
            (
                ProjectionAdmissionFault::TypeProjectionDepthLimit {
                    depth: 9,
                    maximum: 8,
                },
                PackageProjectionAdmissionFaultFacts::TypeProjectionDepthLimit {
                    depth: 9,
                    maximum: 8,
                },
                "lowering_projection_type_projection_depth_limit",
                "type_projection_depth_limit",
            ),
            (
                ProjectionAdmissionFault::TypeProjectionCycle {
                    type_id: 12,
                    owner_span: None,
                    owner_name_span: None,
                },
                PackageProjectionAdmissionFaultFacts::TypeProjectionCycle {
                    type_id: 12,
                    owner_span: None,
                    owner_name_span: None,
                },
                "lowering_projection_type_projection_cycle",
                "type_projection_cycle",
            ),
            (
                ProjectionAdmissionFault::TypeProjectionRecursiveReference { distance: 2 },
                PackageProjectionAdmissionFaultFacts::TypeProjectionRecursiveReference {
                    distance: 2,
                },
                "lowering_projection_type_projection_recursive_reference",
                "type_projection_recursive_reference",
            ),
            (
                ProjectionAdmissionFault::TypeProjectionWidth {
                    actual: 65,
                    maximum: 64,
                },
                PackageProjectionAdmissionFaultFacts::TypeProjectionWidth {
                    actual: 65,
                    maximum: 64,
                },
                "lowering_projection_type_projection_width",
                "type_projection_width",
            ),
        ] {
            let actual = PackageProjectionAdmissionFaultFacts::from(source);
            assert_eq!(actual, expected);
            assert_eq!(actual.kind_tag(), tag);
            let wire = serde_json::to_value(actual).expect("serialize exact closed fault");
            assert_eq!(wire["fault"], wire_fault);
        }

        for (source, expected, wire_tag) in [
            (
                ProjectionSemanticTypeTag::KeyOf,
                PackageTypeTagFact::KeyOf,
                "key_of",
            ),
            (
                ProjectionSemanticTypeTag::IndexedAccess,
                PackageTypeTagFact::IndexedAccess,
                "indexed_access",
            ),
            (
                ProjectionSemanticTypeTag::TypeOf,
                PackageTypeTagFact::TypeOf,
                "type_of",
            ),
        ] {
            let actual = PackageTypeTagFact::from(source);
            assert_eq!(actual, expected);
            assert_eq!(
                serde_json::to_value(actual).expect("serialize exact type tag"),
                wire_tag
            );
        }
    }
}
