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
    Prepare,
    Write,
    Validate,
    Setup,
    Lowering,
    Authority,
}

/// Closed language fact retained for pre-recipe toolchain refusals.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompilerLanguageFact {
    Rust,
    TypeScript,
    Python,
    Go,
    Java,
    CSharp,
    Clang,
}

/// Closed compiler stage fact retained for pre-recipe toolchain refusals.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CompilerStageFact {
    Parse,
    LowerIr,
}

/// Closed native executable family with the host's actual configuration knob.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompilerNativeToolFact {
    Rustc,
    Clang,
    Python,
    TypeScriptCompiler,
    GoCompiler,
    JavaCompiler,
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
        language: CompilerLanguageFact,
        stage: CompilerStageFact,
        selected: CompilerNativeToolFact,
        configured: Option<CompilerNativeToolFact>,
    },
    /// The registry-selected native tool is explicitly absent.
    ToolingUnavailable {
        language: CompilerLanguageFact,
        stage: CompilerStageFact,
        tool: CompilerNativeToolFact,
    },
    /// One exact native or auxiliary tool requirement is absent or failed its bounded probe.
    RequiredTool {
        language: CompilerLanguageFact,
        stage: CompilerStageFact,
        issue: CompilerToolIssue,
    },
    /// Syntax was accepted but the exact closed lowering recipe was unavailable.
    Lowering(PackageLoweringFaultFacts),
    /// Existing compiler authority rejected the source or its semantic projection.
    Authority {
        phase: AuthorityPhaseFact,
        class: AuthorityClassFact,
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
            | Self::RequiredTool { .. } => PackageCompilerFailurePhase::Setup,
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
            } => failure.kind_tag(),
            Self::Authority { class, .. } => match class {
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
    Open,
    Parse,
    Resolve,
    TypeCheck,
    Project,
}

/// Closed authority diagnostic class.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityClassFact {
    Syntax,
    Binding,
    SourceScope,
    Type,
    Authority,
    Projection,
}

/// Safe authority-diagnostic extent; native bytes are deliberately omitted.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CompilerAuthorityDiagnosticFacts {
    pub retained_bytes: u32,
    pub observed_bytes: u64,
    pub truncated: bool,
    /// Closed Python checker cause when available. Native diagnostic text and
    /// paths remain local-only and are not serialized into this summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python_failure: Option<PythonAuthorityFailureKind>,
}

/// Closed compact-lowering cause, with exact TypeScript projection operands
/// and an exhaustive tag for every other existing lowering terminal.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PackageLoweringFaultFacts {
    NoSupportedDeclaration,
    ExtensionAtomUnbound {
        row: u32,
        provisional: u32,
        atom_count: u32,
    },
    ExtensionTypeParametersUnbound {
        row: u32,
        start: u32,
        length: u32,
        element_count: u32,
    },
    FactRejected {
        fact: u64,
        name_len: u64,
        cause: PackageProjectionAdmissionFaultFacts,
    },
    RustFunction,
    RustConstantType,
    RustGenericParameter,
    PythonAssignmentName,
    PythonAssignmentValue,
    ClangDeclarationForm,
    TypeScriptDeclarationForm,
    TypeScriptDeclarationType,
    CSharpDeclarationForm,
    CSharpDeclarationType,
    GoDeclarationForm,
    GoDeclarationType,
    JavaDeclarationForm,
    JavaProjection {
        fault: PackageLanguageProjectionFault,
    },
    GoProjection {
        fault: PackageLanguageProjectionFault,
    },
    TypeScriptProjection {
        fault: PackageTypeScriptProjectionFaultFacts,
    },
    PythonProjection {
        fault: PackageLanguageProjectionFault,
    },
    CSharpProjection {
        fault: PackageLanguageProjectionFault,
    },
    ClangProjection {
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
    ForeignKey {
        start: u32,
        end: u32,
        cause: PackageForeignKeyFaultFacts,
    },
    PackageLineage {
        start: u32,
        end: u32,
        cause: PackageLineageFaultFacts,
    },
    CoordinateOverflow {
        value: u64,
    },
    MissingImportBinding {
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
    JavaImage,
    JavaDepth,
    JavaMalformedType,
    JavaPrimitive,
    JavaAtomUtf8,
    JavaSourceUtf8,
    JavaUtf16Offset,
    JavaUtf16Range,
    JavaOrphanOwner,
    JavaForeignKey,
    JavaSiblingCapacity,
    JavaIndexCapacity,
    GoImage,
    GoIndexCapacity,
    GoDepth,
    GoVariadicWithoutParameter,
    GoAnchor,
    GoListCapacity,
    GoOrphanTarget,
    GoForeignKey,
    GoPackageLineage,
    GoAtomUtf8,
    GoRelativeSpan,
    GoOrphanOwner,
    GoAdmission,
    PythonForeignSpellingUtf8,
    PythonForeignKey,
    PythonPackageLineage,
    PythonContainment,
    CSharpImage,
    CSharpDepth,
    CSharpNameSpan,
    CSharpOwnerOrder,
    CSharpAnchor,
    CSharpForeign,
    CSharpAttributeCapacity,
    CSharpIndexCapacity,
    CSharpHeterogeneousArrayRank,
    CSharpResultName,
    CSharpAdmission,
    ClangSpan,
    ClangNameless,
    ClangAnchor,
    ClangIndexCapacity,
    ClangForeignOverride,
    ClangForeignReference,
    ClangIllegalQualifierTarget,
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
    Tag {
        actual: u32,
    },
    ReservedPayload {
        tag: u8,
        payload0: u32,
        payload1: u32,
    },
    ArityOverflow {
        tag: u8,
        payload0: u32,
        payload1: u32,
    },
    Arity {
        tag: u8,
        expected: u32,
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
    Tag {
        actual: u8,
    },
    ReservedCell {
        tag: PackageTypeTagFact,
        cell: PackageTypeCellFact,
        actual: u32,
    },
    MissingCell {
        tag: PackageTypeTagFact,
        cell: PackageTypeCellFact,
    },
    Reason {
        actual: u32,
    },
    PrimitiveShape {
        actual: u32,
    },
    CvQualifiers {
        actual: u32,
    },
    Width {
        actual: u32,
    },
    ChildCount {
        tag: PackageTypeTagFact,
        min: u32,
        max: u32,
        actual: u32,
    },
    ChildNameForbidden {
        tag: PackageTypeTagFact,
        position: u32,
    },
    ChildNameRequired {
        tag: PackageTypeTagFact,
        position: u32,
    },
    ChildFlagsForbidden {
        tag: PackageTypeTagFact,
        position: u32,
        actual: u8,
    },
    VariadicParameter {
        position: u32,
        actual: u8,
    },
    ChildTextForbidden {
        tag: PackageTypeTagFact,
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
    SelfType,
    Primitive,
    Tuple,
    Slice,
    Array,
    Union,
    Intersection,
    Never,
    Any,
    Unknown,
    Nominal,
    Apply,
    TypeVar,
    Wildcard,
    FunctionPointer,
    Annotated,
    Conditional,
    Mapped,
    TemplateLiteral,
    AnonymousRecord,
    ImplTrait,
    DynTrait,
    Inferred,
    QualifiedPath,
    Map,
    Channel,
    ArraySequence,
    ArrayRectangular,
    ArrayFixed,
    ArrayConstExpression,
    ArrayIncomplete,
    CQualified,
    KeyOf,
    IndexedAccess,
    TypeOf,
}

/// Closed type-record cell operand.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageTypeCellFact {
    Payload0,
    Payload1,
    Text,
    Text2,
    Nominal,
}

/// Exact portable projection-admission fault. Each variant requires only its
/// own operands; no optional numeric coordinate bag is admitted.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "fault", rename_all = "snake_case", deny_unknown_fields)]
pub enum PackageProjectionAdmissionFaultFacts {
    EmptyName,
    AnonymousCallableAnchor {
        cause: PackageAnonymousCallableAnchorFaultFacts,
    },
    AnonymousCallableAnchorPoolCapacity {
        pool: PackageAnonymousCallableAnchorPoolFact,
        used: u64,
        requested: u64,
        capacity: u64,
    },
    Capacity,
    ChildCapacity,
    ProductChildPoolCapacity {
        used: u64,
        requested: u64,
        capacity: u64,
    },
    Constructor {
        cause: PackageProjectionConstructorFaultFacts,
    },
    ChildRole {
        position: u64,
        expected: u8,
        actual: u8,
    },
    ChildTarget {
        position: u64,
        target: u32,
        fact_count: u64,
    },
    TypeRecord {
        cause: PackageProjectionSemanticTypeFaultFacts,
    },
    TypeChild {
        position: u64,
        cause: PackageProjectionSemanticTypeFaultFacts,
    },
    TypeChildTarget {
        position: u64,
        target: u32,
        fact_count: u64,
    },
    TypeChildCapacity,
    TypeChildPoolCapacity {
        lane: u8,
        used: u64,
        requested: u64,
        capacity: u64,
    },
    TypeRowCapacity,
    ComputedRowCapacity,
    TypeProjectionDepthLimit {
        depth: u64,
        maximum: u64,
    },
    TypeProjectionCycle {
        type_id: u32,
    },
    TypeProjectionRecursiveReference {
        distance: u32,
    },
    TypeProjectionWidth {
        actual: u64,
        maximum: u64,
    },
    OccurrenceOwner {
        owner: u32,
        fact_count: u64,
    },
    OccurrenceCapacity,
    DocOwner {
        owner: u32,
        fact_count: u64,
    },
    DocCapacity,
    ExtensionAtomCapacity,
    TypeParameterCapacity,
    TypeParameterBoundCapacity {
        requested: u64,
        available: u64,
    },
    RefListCapacity,
    RefListElements,
    RefTarget {
        lane: u8,
        raw: u32,
        fact_count: u64,
    },
    SourceSpan {
        entity: u32,
        start: u32,
        end: u32,
        source_len: u32,
    },
    ConflictingSourceSpan {
        entity: u32,
        existing_start: u32,
        existing_end: u32,
        requested_start: u32,
        requested_end: u32,
    },
    ConflictingParentage {
        entity: u32,
        existing: PackageParentageFact,
        requested: PackageParentageFact,
    },
    ConflictingMemberInventory {
        entity: u32,
        existing_count: u64,
        requested_count: u64,
        first_difference: u64,
        existing_member: Option<u32>,
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
    Unavailable,
    Root,
    Bound { parent: u32 },
    UnrepresentedAuthorityOwner { identity: [u8; 16] },
}

/// Closed foreign-key grammar fault.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageForeignKeyFaultFacts {
    EmptyPath,
    BackslashInPath,
}

/// Closed package-lineage grammar fault.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageLineageFaultFacts {
    EmptyEcosystem,
    EmptyPackage,
    SeparatorInEcosystem,
    SeparatorInPackage,
    BackslashEcosystem,
    BackslashPackage,
    BackslashInvalid { segment: u8 },
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
            F::TypeProjectionCycle { type_id } => Self::TypeProjectionCycle { type_id },
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
                ProjectionAdmissionFault::TypeProjectionCycle { type_id: 12 },
                PackageProjectionAdmissionFaultFacts::TypeProjectionCycle { type_id: 12 },
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
