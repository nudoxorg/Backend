//! C# Roslyn native authority adapter.
#![forbid(unsafe_code)]

use backend_compile::{
    Authority, AuthorityError, AuthorityIdentity, DiscoverySnapshot, Extraction, Input, InputKind,
    InputManifest, NativeRequestInput, NativeTemplate, ProcessLimits, ProtocolDescriptor,
    SessionKey, SupervisedCommand, default_native_limits, extract_native_checked, native_input,
    native_semantic_evidence, native_semantic_input, typed_of,
};
use std::path::Path;

const LANGUAGE: &str = "csharp";

/// Builds the zero-toolchain local C# syntax frontend.
///
/// # Errors
/// Returns an error when the embedded grammar query cannot be admitted.
pub fn syntax_frontend() -> Result<backend_compile::SyntaxFrontend, backend_compile::SyntaxError> {
    backend_compile::SyntaxFrontend::new(
        backend_compile::SourceLanguage::CSharp,
        b"tree-sitter-c-sharp-0.23.5/tags-v1",
        vec![backend_compile::GrammarVariant::new(
            &["cs"],
            tree_sitter_c_sharp::LANGUAGE.into(),
            tree_sitter_c_sharp::TAGS_QUERY,
        )?],
    )
}

/// Roslyn authority configuration with explicit source-bound helper identity.
pub struct CSharpFrontend {
    source: Vec<u8>,
    dotnet: String,
    helper: String,
    config: String,
    template: Option<NativeTemplate>,
}

impl CSharpFrontend {
    /// Creates a manifest-only Roslyn configuration.
    ///
    /// The helper path is retained as a dependency input, but no executable
    /// is trusted for semantic output until [`Self::with_helper`] is used.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn new(
        source: Vec<u8>,
        dotnet: impl Into<String>,
        helper: impl Into<String>,
        config: impl Into<String>,
    ) -> Result<Self, AuthorityError> {
        let dotnet = dotnet.into();
        let helper = helper.into();
        validate_absolute(&dotnet, "dotnet executable")?;
        validate_absolute(&helper, "Roslyn authority helper")?;
        Ok(Self {
            source,
            dotnet,
            helper,
            config: config.into(),
            template: None,
        })
    }

    /// Creates a cold Roslyn authority using the explicit helper executable.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn with_helper(
        source: Vec<u8>,
        dotnet: impl Into<String>,
        helper: impl Into<String>,
        config: impl Into<String>,
    ) -> Result<Self, AuthorityError> {
        Self::with_mode_and_limits(
            source,
            dotnet,
            helper,
            config,
            false,
            default_native_limits()?,
        )
    }

    /// Creates a persistent Roslyn authority using an explicit helper.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn with_persistent_helper(
        source: Vec<u8>,
        dotnet: impl Into<String>,
        helper: impl Into<String>,
        config: impl Into<String>,
    ) -> Result<Self, AuthorityError> {
        Self::with_mode_and_limits(
            source,
            dotnet,
            helper,
            config,
            true,
            default_native_limits()?,
        )
    }

    /// Creates a cold Roslyn authority with an explicit process budget.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn with_helper_and_limits(
        source: Vec<u8>,
        dotnet: impl Into<String>,
        helper: impl Into<String>,
        config: impl Into<String>,
        limits: ProcessLimits,
    ) -> Result<Self, AuthorityError> {
        Self::with_mode_and_limits(source, dotnet, helper, config, false, limits)
    }

    /// Creates a persistent Roslyn authority with an explicit process budget.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn with_persistent_helper_and_limits(
        source: Vec<u8>,
        dotnet: impl Into<String>,
        helper: impl Into<String>,
        config: impl Into<String>,
        limits: ProcessLimits,
    ) -> Result<Self, AuthorityError> {
        Self::with_mode_and_limits(source, dotnet, helper, config, true, limits)
    }

    fn with_mode_and_limits(
        source: Vec<u8>,
        dotnet: impl Into<String>,
        helper: impl Into<String>,
        config: impl Into<String>,
        persistent: bool,
        limits: ProcessLimits,
    ) -> Result<Self, AuthorityError> {
        let dotnet = dotnet.into();
        let helper = helper.into();
        let config = config.into();
        validate_absolute(&dotnet, "dotnet executable")?;
        validate_absolute(&helper, "Roslyn authority helper")?;
        let template = NativeTemplate::new(
            LANGUAGE,
            &helper,
            &dotnet,
            backend_compile::native_executable_id(Path::new(&dotnet)),
            if persistent {
                ProtocolDescriptor::persistent()
            } else {
                ProtocolDescriptor::cold()
            },
            limits,
        )?;
        Ok(Self {
            source,
            dotnet,
            helper,
            config,
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

    /// Returns the configured helper path.
    #[must_use]
    pub fn helper(&self) -> &Path {
        Path::new(&self.helper)
    }

    fn manifest(&self) -> Result<InputManifest, AuthorityError> {
        InputManifest::new(vec![
            Input::new(InputKind::Source, "source.cs", &self.source).map_err(discovery)?,
            Input::new(
                InputKind::Toolchain,
                "dotnet",
                &backend_compile::native_executable_evidence(Path::new(&self.dotnet)),
            )
            .map_err(discovery)?,
            Input::new(
                InputKind::Dependency,
                "roslyn-helper",
                &backend_compile::native_executable_evidence(Path::new(&self.helper)),
            )
            .map_err(discovery)?,
            Input::new(
                InputKind::Configuration,
                "roslyn-options",
                self.config.as_bytes(),
            )
            .map_err(discovery)?,
            Input::absent(
                InputKind::NegativeDependency,
                "reference-assemblies/optional",
            )
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

impl Authority for CSharpFrontend {
    fn identity(&self) -> AuthorityIdentity {
        AuthorityIdentity {
            producer: typed_of(b"backend-frontend-csharp-v3"),
            toolchain: backend_compile::native_executable_id(Path::new(&self.dotnet)),
            contract: typed_of(b"native-fact-envelope-v1/csharp"),
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
        let fields = request_inputs(&self.source, &self.dotnet, &self.helper, &self.config)?;
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
    dotnet: &str,
    helper: &str,
    config: &str,
) -> Result<Vec<NativeRequestInput>, AuthorityError> {
    Ok(vec![
        native_input("source.cs", source.to_vec())?,
        native_input("roslyn-options", config.as_bytes().to_vec())?,
        native_input(
            "dotnet",
            backend_compile::native_executable_evidence(Path::new(dotnet)),
        )?,
        native_input(
            "roslyn-helper",
            backend_compile::native_executable_evidence(Path::new(helper)),
        )?,
        native_input("reference-assemblies/optional", b"absent".to_vec())?,
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
