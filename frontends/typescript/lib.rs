//! TypeScript OXC/checker native authority adapter.
#![forbid(unsafe_code)]

use backend_compile::{
    Authority, AuthorityError, AuthorityIdentity, DiscoverySnapshot, Extraction, Input, InputKind,
    InputManifest, NativeRequestInput, NativeTemplate, ProcessLimits, ProtocolDescriptor,
    SessionKey, SupervisedCommand, default_native_limits, extract_native_checked, native_input,
    native_semantic_evidence, native_semantic_input, typed_of,
};
use std::path::Path;

const LANGUAGE: &str = "typescript";

/// Builds the zero-toolchain local TypeScript, TSX, and JavaScript syntax frontend.
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
    profile: String,
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
    ) -> Result<Self, AuthorityError> {
        let node = node.into();
        let typescript = typescript.into();
        validate_absolute(&node, "Node executable")?;
        validate_absolute(&typescript, "TypeScript authority helper")?;
        Ok(Self {
            source,
            node,
            typescript,
            profile: profile.into(),
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
    ) -> Result<Self, AuthorityError> {
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
    ) -> Result<Self, AuthorityError> {
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
    ) -> Result<Self, AuthorityError> {
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
    ) -> Result<Self, AuthorityError> {
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
    ) -> Result<Self, AuthorityError> {
        let node = node.into();
        let typescript = typescript.into();
        let profile = profile.into();
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

    fn manifest(&self) -> Result<InputManifest, AuthorityError> {
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
                self.profile.as_bytes(),
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
            producer: typed_of(b"backend-frontend-typescript-v3"),
            toolchain: backend_compile::native_executable_id(Path::new(&self.node)),
            contract: typed_of(b"native-fact-envelope-v1/typescript"),
        }
    }

    fn discover(&self) -> Result<DiscoverySnapshot, AuthorityError> {
        Ok(DiscoverySnapshot::new(self.manifest()?, 0))
    }

    fn extract(
        &self,
        snapshot: &DiscoverySnapshot,
        key: SessionKey,
    ) -> Result<Extraction, AuthorityError> {
        let fields = request_inputs(
            &self.source,
            &self.node,
            &self.typescript,
            &self.profile,
            &self.package,
        )?;
        extract_native_checked(
            self.identity(),
            LANGUAGE,
            snapshot,
            key,
            self.template.as_ref(),
            fields,
            self.manifest()?.digest(),
        )
    }
}

fn request_inputs(
    source: &[u8],
    node: &str,
    typescript: &str,
    profile: &str,
    package: &[u8],
) -> Result<Vec<NativeRequestInput>, AuthorityError> {
    Ok(vec![
        native_input("index.ts", source.to_vec())?,
        native_input("ts-profile", profile.as_bytes().to_vec())?,
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

fn validate_absolute(path: &str, label: &str) -> Result<(), AuthorityError> {
    if !Path::new(path).is_absolute() {
        return Err(AuthorityError::Discovery(format!(
            "{label} must be absolute"
        )));
    }
    Ok(())
}

fn discovery<E: std::fmt::Display>(error: E) -> AuthorityError {
    AuthorityError::Discovery(error.to_string())
}

/// Honest native authority session capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionCapability {
    /// Identity that invalidates a session after authority changes.
    pub reset_key: backend_compile::SessionId,
    /// Whether a configured helper advertises persistent sessions.
    pub persistent: bool,
}
