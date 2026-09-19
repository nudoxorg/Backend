//! TypeScript OXC/checker native authority adapter.
#![forbid(unsafe_code)]

mod authority;
mod checker;
mod coordinate;
mod error;
mod package;

#[path = "src/legacy/mod.rs"]
pub mod legacy;

pub use authority::{
    OxcDeclaration, OxcDeclarationKind, OxcModule, SyntaxMappedModifier, analyze,
    syntax_mapped_modifier, with_analysis,
};
pub use checker::{
    BoundDeclaration, BoundNarrowing, BoundReference, Checker, CheckerError, CheckerIndex,
    Declaration, ExplicitTypeScriptChecker, LiteralBase, MappedModifier, Narrowing, ObjectMember,
    Origin, Parameter, Reference, Report, TemplatePart, TypeScriptCheckerProgram,
    TypeScriptCheckerProgramError, TypeScriptCheckerProgramView, TypeScriptModuleRoot,
    TypeScriptModuleRootView, TypeTree, source_digest,
};
pub use coordinate::{CoordinateError, Utf8Span, Utf8ToUtf16Cursor, Utf16Span};
pub use error::{OxcAuthorityError, OxcAuthorityError as AuthorityError};
pub use package::{
    LocatedPackage, MAX_TARBALL_MEMBERS, MAX_TARBALL_UNCOMPRESSED_BYTES, PackageError, PackagePurl,
    PackagePurlError, RegistryMetadata, TarballMember, TarballMemberKind, decode_packument,
    locate_package, read_tarball,
};

use backend_compile::{
    Authority, AuthorityError as CompileAuthorityError, AuthorityIdentity, DiscoverySnapshot,
    Extraction, FactKeySchema, FactKind, FactRecord, FactValueSchema, Input, InputKind,
    InputManifest, NativeRecord, NativeRecordKind, NativeRequestInput, NativeSemanticAdapter,
    NativeSemanticRequest, NativeTemplate, ProcessLimits, ProtocolDescriptor, SessionKey,
    SupervisedCommand, TypeScriptSource, default_native_limits, extract_native_with_adapter,
    native_input, native_semantic_evidence, native_semantic_input, typed_of,
};
use std::{fmt, path::Path};

const LANGUAGE: &str = "typescript";

struct TypeScriptSemanticAdapter;

#[derive(Clone, Copy)]
struct ParsedTypeScriptRecord<'record> {
    kind: NativeRecordKind,
    key: &'record str,
    value: &'record str,
}

#[derive(Clone, Copy)]
struct AdmittedTypeScriptRecord<'record>(ParsedTypeScriptRecord<'record>);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TypeScriptSemanticError {
    NonUtf8Value,
    EmptyValue,
    InvalidEdge,
    InvalidDependencyState,
}

impl fmt::Display for TypeScriptSemanticError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NonUtf8Value => "TypeScript semantic value is not UTF-8",
            Self::EmptyValue => "TypeScript semantic value is empty",
            Self::InvalidEdge => "TypeScript semantic edge has no source and target",
            Self::InvalidDependencyState => "TypeScript dependency state is invalid",
        })
    }
}

impl std::error::Error for TypeScriptSemanticError {}

impl NativeSemanticAdapter for TypeScriptSemanticAdapter {
    type Parsed<'record> = ParsedTypeScriptRecord<'record>;
    type Admitted<'record> = AdmittedTypeScriptRecord<'record>;
    type Error = TypeScriptSemanticError;

    fn parse<'record>(
        &'record self,
        record: &'record NativeRecord,
    ) -> Result<Self::Parsed<'record>, Self::Error> {
        let value = std::str::from_utf8(record.value())
            .map_err(|_| TypeScriptSemanticError::NonUtf8Value)?;
        Ok(ParsedTypeScriptRecord {
            kind: record.kind(),
            key: record.key(),
            value,
        })
    }

    fn admit<'record>(
        &'record self,
        parsed: Self::Parsed<'record>,
    ) -> Result<Self::Admitted<'record>, Self::Error> {
        if parsed.value.is_empty() {
            return Err(TypeScriptSemanticError::EmptyValue);
        }
        match parsed.kind {
            NativeRecordKind::Edge
                if parsed
                    .key
                    .split_once("->")
                    .is_none_or(|(source, target)| source.is_empty() || target.is_empty()) =>
            {
                Err(TypeScriptSemanticError::InvalidEdge)
            }
            NativeRecordKind::Dependency if parsed.value != "present" => {
                Err(TypeScriptSemanticError::InvalidDependencyState)
            }
            NativeRecordKind::NegativeDependency if parsed.value != "absent" => {
                Err(TypeScriptSemanticError::InvalidDependencyState)
            }
            _ => Ok(AdmittedTypeScriptRecord(parsed)),
        }
    }

    fn lower<'record>(
        &'record self,
        admitted: Self::Admitted<'record>,
    ) -> FactRecord<FactKeySchema, FactValueSchema> {
        let record = admitted.0;
        FactRecord::new(
            fact_kind(record.kind),
            record.key.as_bytes().to_vec(),
            record.value.as_bytes().to_vec(),
        )
    }
}

const fn fact_kind(kind: NativeRecordKind) -> FactKind {
    match kind {
        NativeRecordKind::Declaration => FactKind::Declaration,
        NativeRecordKind::Type => FactKind::Type,
        NativeRecordKind::Edge => FactKind::Edge,
        NativeRecordKind::Diagnostic => FactKind::Diagnostic,
        NativeRecordKind::Dependency => FactKind::Dependency,
        NativeRecordKind::NegativeDependency => FactKind::NegativeDependency,
    }
}

/// Builds the zero-toolchain local TypeScript, TSX, and JavaScript syntax frontend.
///
/// This structural baseline never claims native semantic authority.
///
/// # Errors
/// Returns an error when an embedded grammar query cannot be admitted.
pub fn syntax_frontend() -> Result<backend_compile::SyntaxFrontend, backend_compile::SyntaxError> {
    let tags = format!(
        "{}\n{}",
        tree_sitter_typescript::TAGS_QUERY,
        r"
(function_declaration name: (identifier) @name) @definition.function
(class_declaration name: (type_identifier) @name) @definition.class
(method_definition name: (property_identifier) @name) @definition.method
(type_alias_declaration name: (type_identifier) @name) @definition.type
(enum_declaration name: (identifier) @name) @definition.type
(lexical_declaration (variable_declarator name: (identifier) @name)) @definition.constant
"
    );
    backend_compile::SyntaxFrontend::new(
        backend_compile::SourceLanguage::TypeScript,
        b"tree-sitter-typescript-0.23.2/tags-v2",
        vec![
            backend_compile::GrammarVariant::new(
                &["ts", "js", "mjs", "cjs"],
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                &tags,
            )?,
            backend_compile::GrammarVariant::new(
                &["tsx", "jsx"],
                tree_sitter_typescript::LANGUAGE_TSX.into(),
                &tags,
            )?,
        ],
    )
}

/// TypeScript authority configuration with package and Node identity.
pub struct TypeScriptFrontend {
    source: Vec<u8>,
    node: String,
    typescript: String,
    profile: TypeScriptSource,
    package: Vec<u8>,
    template: Option<NativeTemplate>,
}

impl TypeScriptFrontend {
    /// Creates a manifest-only TypeScript configuration.
    ///
    /// The TypeScript module itself does not emit the backend authority
    /// payload. Call [`Self::with_helper`] to select an explicit checker
    /// adapter.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn new(
        source: Vec<u8>,
        node: impl Into<String>,
        typescript: impl Into<String>,
        profile: impl Into<String>,
        package: Vec<u8>,
    ) -> Result<Self, CompileAuthorityError> {
        let node = node.into();
        let typescript = typescript.into();
        let profile = profile.into();
        let profile = parse_profile(&profile)?;
        validate_absolute(&node, "Node executable")?;
        validate_absolute(&typescript, "TypeScript authority helper")?;
        Ok(Self {
            source,
            node,
            typescript,
            profile,
            package,
            template: None,
        })
    }

    /// Creates a cold TypeScript authority using an explicit absolute helper.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn with_helper(
        source: Vec<u8>,
        node: impl Into<String>,
        typescript: impl Into<String>,
        profile: impl Into<String>,
        package: Vec<u8>,
    ) -> Result<Self, CompileAuthorityError> {
        Self::with_mode_and_limits(
            source,
            node,
            typescript,
            profile,
            package,
            false,
            default_native_limits()?,
        )
    }

    /// Creates a persistent TypeScript authority using an explicit helper.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn with_persistent_helper(
        source: Vec<u8>,
        node: impl Into<String>,
        typescript: impl Into<String>,
        profile: impl Into<String>,
        package: Vec<u8>,
    ) -> Result<Self, CompileAuthorityError> {
        Self::with_mode_and_limits(
            source,
            node,
            typescript,
            profile,
            package,
            true,
            default_native_limits()?,
        )
    }

    /// Creates a cold TypeScript authority with an explicit process budget.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn with_helper_and_limits(
        source: Vec<u8>,
        node: impl Into<String>,
        typescript: impl Into<String>,
        profile: impl Into<String>,
        package: Vec<u8>,
        limits: ProcessLimits,
    ) -> Result<Self, CompileAuthorityError> {
        Self::with_mode_and_limits(source, node, typescript, profile, package, false, limits)
    }

    /// Creates a persistent TypeScript authority with an explicit process budget.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn with_persistent_helper_and_limits(
        source: Vec<u8>,
        node: impl Into<String>,
        typescript: impl Into<String>,
        profile: impl Into<String>,
        package: Vec<u8>,
        limits: ProcessLimits,
    ) -> Result<Self, CompileAuthorityError> {
        Self::with_mode_and_limits(source, node, typescript, profile, package, true, limits)
    }

    fn with_mode_and_limits(
        source: Vec<u8>,
        node: impl Into<String>,
        typescript: impl Into<String>,
        profile: impl Into<String>,
        package: Vec<u8>,
        persistent: bool,
        limits: ProcessLimits,
    ) -> Result<Self, CompileAuthorityError> {
        let node = node.into();
        let typescript = typescript.into();
        let profile = profile.into();
        let profile = parse_profile(&profile)?;
        validate_absolute(&node, "Node executable")?;
        validate_absolute(&typescript, "TypeScript authority helper")?;
        let template = NativeTemplate::new(
            LANGUAGE,
            &typescript,
            &node,
            backend_compile::native_executable_id(Path::new(&node)),
            if persistent {
                ProtocolDescriptor::persistent()
            } else {
                ProtocolDescriptor::cold()
            },
            limits,
        )?;
        Ok(Self {
            source,
            node,
            typescript,
            profile,
            package,
            template: Some(template),
        })
    }

    /// Returns the authority capability and reset identity.
    pub fn session_capability(&self) -> SessionCapability {
        SessionCapability {
            reset_key: self.identity().digest(),
            persistent: self
                .template
                .as_ref()
                .is_some_and(NativeTemplate::persistent),
        }
    }

    /// Returns the checked helper command, when one was configured.
    #[must_use]
    pub fn native_command(&self) -> Option<&SupervisedCommand> {
        self.template.as_ref().map(NativeTemplate::command)
    }

    /// Returns the configured TypeScript helper path.
    #[must_use]
    pub fn helper(&self) -> &Path {
        Path::new(&self.typescript)
    }

    fn manifest(&self) -> Result<InputManifest, CompileAuthorityError> {
        InputManifest::new(vec![
            Input::new(InputKind::Source, "index.ts", &self.source).map_err(discovery)?,
            Input::new(
                InputKind::Toolchain,
                "node",
                &backend_compile::native_executable_evidence(Path::new(&self.node)),
            )
            .map_err(discovery)?,
            Input::new(
                InputKind::Dependency,
                "typescript",
                &backend_compile::native_executable_evidence(Path::new(&self.typescript)),
            )
            .map_err(discovery)?,
            Input::new(
                InputKind::Configuration,
                "ts-profile",
                self.profile.name().as_bytes(),
            )
            .map_err(discovery)?,
            Input::new(InputKind::Dependency, "package-snapshot", &self.package)
                .map_err(discovery)?,
            Input::absent(InputKind::NegativeDependency, "node_modules/optional")
                .map_err(discovery)?,
            Input::new(
                InputKind::Configuration,
                "semantic-fact-schema",
                &native_semantic_evidence(),
            )
            .map_err(discovery)?,
        ])
        .map_err(discovery)
    }
}

impl Authority for TypeScriptFrontend {
    fn identity(&self) -> AuthorityIdentity {
        AuthorityIdentity {
            producer: typed_of(b"backend-frontend-typescript-v4"),
            toolchain: backend_compile::native_executable_id(Path::new(&self.node)),
            contract: typed_of(b"native-semantic-adapter-v2/typescript"),
        }
    }

    fn discover(&self) -> Result<DiscoverySnapshot, CompileAuthorityError> {
        Ok(DiscoverySnapshot::new(self.manifest()?, 0))
    }

    fn extract(
        &self,
        snapshot: &DiscoverySnapshot,
        key: SessionKey,
    ) -> Result<Extraction, CompileAuthorityError> {
        let fields = request_inputs(
            &self.source,
            &self.node,
            &self.typescript,
            self.profile,
            &self.package,
        )?;
        extract_native_with_adapter(
            NativeSemanticRequest::new(
                self.identity(),
                LANGUAGE,
                snapshot,
                key,
                self.template.as_ref(),
                fields,
                self.manifest()?.digest(),
            ),
            &TypeScriptSemanticAdapter,
        )
    }
}

fn request_inputs(
    source: &[u8],
    node: &str,
    typescript: &str,
    profile: TypeScriptSource,
    package: &[u8],
) -> Result<Vec<NativeRequestInput>, CompileAuthorityError> {
    Ok(vec![
        native_input("index.ts", source.to_vec())?,
        native_input("ts-profile", profile.name().as_bytes().to_vec())?,
        native_input("package-snapshot", package.to_vec())?,
        native_input(
            "node",
            backend_compile::native_executable_evidence(Path::new(node)),
        )?,
        native_input(
            "typescript",
            backend_compile::native_executable_evidence(Path::new(typescript)),
        )?,
        native_input("node_modules/optional", b"absent".to_vec())?,
        native_semantic_input()?,
    ])
}

fn validate_absolute(path: &str, label: &str) -> Result<(), CompileAuthorityError> {
    if !Path::new(path).is_absolute() {
        return Err(CompileAuthorityError::Discovery(format!(
            "{label} must be absolute"
        )));
    }
    Ok(())
}

fn parse_profile(profile: &str) -> Result<TypeScriptSource, CompileAuthorityError> {
    TypeScriptSource::try_from(profile).map_err(discovery)
}

fn discovery<E: fmt::Display>(error: E) -> CompileAuthorityError {
    CompileAuthorityError::Discovery(error.to_string())
}

/// Honest native authority session capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionCapability {
    /// Identity that invalidates a session after authority changes.
    pub reset_key: backend_compile::SessionId,
    /// Whether a configured helper advertises persistent sessions.
    pub persistent: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn semantic_adapter_rejects_unproved_edge_shape() -> Result<(), Box<dyn Error>> {
        let record = NativeRecord::new(NativeRecordKind::Edge, "source", b"reference".to_vec())?;
        let parsed = TypeScriptSemanticAdapter.parse(&record)?;
        assert!(matches!(
            TypeScriptSemanticAdapter.admit(parsed),
            Err(TypeScriptSemanticError::InvalidEdge)
        ));
        Ok(())
    }

    #[test]
    fn semantic_adapter_lowers_only_admitted_types() -> Result<(), Box<dyn Error>> {
        let record = NativeRecord::new(NativeRecordKind::Type, "answer", b"number".to_vec())?;
        let parsed = TypeScriptSemanticAdapter.parse(&record)?;
        let admitted = TypeScriptSemanticAdapter.admit(parsed)?;
        let fact = TypeScriptSemanticAdapter.lower(admitted);
        assert_eq!(fact.kind(), FactKind::Type);
        assert_eq!(fact.key_bytes(), b"answer");
        assert_eq!(fact.value(), b"number");
        Ok(())
    }
}
