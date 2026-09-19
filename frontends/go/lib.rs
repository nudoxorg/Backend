//! Go `go/packages` native authority adapter.
#![forbid(unsafe_code)]

use backend_compile::{
    Authority, AuthorityError, AuthorityIdentity, DiscoverySnapshot, Extraction, Input, InputKind,
    InputManifest, NativeRequestInput, NativeTemplate, ProcessLimits, ProtocolDescriptor,
    SessionKey, SupervisedCommand, default_native_limits, extract_native_checked, native_input,
    native_semantic_evidence, native_semantic_input, typed_of,
};
use std::path::Path;

const LANGUAGE: &str = "go";

/// Builds the zero-toolchain local Go syntax frontend.
///
/// # Errors
/// Returns an error when the embedded grammar query cannot be admitted.
pub fn syntax_frontend() -> Result<backend_compile::SyntaxFrontend, backend_compile::SyntaxError> {
    backend_compile::SyntaxFrontend::new(
        backend_compile::SourceLanguage::Go,
        b"tree-sitter-go-0.25.0/tags-v1",
        vec![backend_compile::GrammarVariant::new(
            &["go"],
            tree_sitter_go::LANGUAGE.into(),
            tree_sitter_go::TAGS_QUERY,
        )?],
    )
}

/// Go module authority with explicit module/build inputs.
pub struct GoFrontend {
    source: Vec<u8>,
    go: String,
    modfile: Vec<u8>,
    tags: String,
    helper: Option<String>,
    template: Option<NativeTemplate>,
}

impl GoFrontend {
    /// Creates a manifest-only Go configuration.
    ///
    /// The Go compiler does not emit the backend authority payload. Call
    /// [`Self::with_helper`] to select an explicit `go/packages` adapter.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn new(
        source: Vec<u8>,
        go: impl Into<String>,
        modfile: Vec<u8>,
        tags: impl Into<String>,
    ) -> Result<Self, AuthorityError> {
        let go = go.into();
        validate_absolute(&go, "Go executable")?;
        Ok(Self {
            source,
            go,
            modfile,
            tags: tags.into(),
            helper: None,
            template: None,
        })
    }

    /// Creates a cold Go authority using an explicit absolute helper.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn with_helper(
        source: Vec<u8>,
        go: impl Into<String>,
        helper: impl Into<String>,
        modfile: Vec<u8>,
        tags: impl Into<String>,
    ) -> Result<Self, AuthorityError> {
        Self::with_mode_and_limits(
            source,
            go,
            helper,
            modfile,
            tags,
            false,
            default_native_limits()?,
        )
    }

    /// Creates a persistent Go authority using an explicit helper.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn with_persistent_helper(
        source: Vec<u8>,
        go: impl Into<String>,
        helper: impl Into<String>,
        modfile: Vec<u8>,
        tags: impl Into<String>,
    ) -> Result<Self, AuthorityError> {
        Self::with_mode_and_limits(
            source,
            go,
            helper,
            modfile,
            tags,
            true,
            default_native_limits()?,
        )
    }

    /// Creates a cold Go authority with an explicit process budget.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn with_helper_and_limits(
        source: Vec<u8>,
        go: impl Into<String>,
        helper: impl Into<String>,
        modfile: Vec<u8>,
        tags: impl Into<String>,
        limits: ProcessLimits,
    ) -> Result<Self, AuthorityError> {
        Self::with_mode_and_limits(source, go, helper, modfile, tags, false, limits)
    }

    /// Creates a persistent Go authority with an explicit process budget.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn with_persistent_helper_and_limits(
        source: Vec<u8>,
        go: impl Into<String>,
        helper: impl Into<String>,
        modfile: Vec<u8>,
        tags: impl Into<String>,
        limits: ProcessLimits,
    ) -> Result<Self, AuthorityError> {
        Self::with_mode_and_limits(source, go, helper, modfile, tags, true, limits)
    }

    fn with_mode_and_limits(
        source: Vec<u8>,
        go: impl Into<String>,
        helper: impl Into<String>,
        modfile: Vec<u8>,
        tags: impl Into<String>,
        persistent: bool,
        limits: ProcessLimits,
    ) -> Result<Self, AuthorityError> {
        let go = go.into();
        let helper = helper.into();
        let tags = tags.into();
        validate_absolute(&go, "Go executable")?;
        validate_absolute(&helper, "Go authority helper")?;
        let template = NativeTemplate::new(
            LANGUAGE,
            &helper,
            &go,
            backend_compile::native_executable_id(Path::new(&go)),
            if persistent {
                ProtocolDescriptor::persistent()
            } else {
                ProtocolDescriptor::cold()
            },
            limits,
        )?;
        Ok(Self {
            source,
            go,
            modfile,
            tags,
            helper: Some(helper),
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

    /// Returns the configured helper path, when one was selected.
    #[must_use]
    pub fn helper(&self) -> Option<&Path> {
        self.template.as_ref().map(NativeTemplate::helper)
    }

    fn manifest(&self) -> Result<InputManifest, AuthorityError> {
        let helper = match &self.helper {
            Some(path) => Input::new(
                InputKind::Dependency,
                "authority-helper",
                &backend_compile::native_executable_evidence(Path::new(path)),
            )
            .map_err(discovery)?,
            None => Input::absent(InputKind::Dependency, "authority-helper").map_err(discovery)?,
        };
        InputManifest::new(vec![
            Input::new(InputKind::Source, "module/source.go", &self.source).map_err(discovery)?,
            Input::new(InputKind::Configuration, "go.mod", &self.modfile).map_err(discovery)?,
            Input::new(
                InputKind::Toolchain,
                "go",
                &backend_compile::native_executable_evidence(Path::new(&self.go)),
            )
            .map_err(discovery)?,
            Input::new(InputKind::Configuration, "build-tags", self.tags.as_bytes())
                .map_err(discovery)?,
            Input::absent(InputKind::NegativeDependency, "vendor/optional").map_err(discovery)?,
            Input::new(
                InputKind::Configuration,
                "semantic-fact-schema",
                &native_semantic_evidence(),
            )
            .map_err(discovery)?,
            helper,
        ])
        .map_err(discovery)
    }
}

impl Authority for GoFrontend {
    fn identity(&self) -> AuthorityIdentity {
        AuthorityIdentity {
            producer: typed_of(b"backend-frontend-go-v3"),
            toolchain: backend_compile::native_executable_id(Path::new(&self.go)),
            contract: typed_of(b"native-fact-envelope-v1/go"),
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
            &self.go,
            self.helper.as_deref().unwrap_or("unsupported"),
            &self.modfile,
            &self.tags,
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
    go: &str,
    helper: &str,
    modfile: &[u8],
    tags: &str,
) -> Result<Vec<NativeRequestInput>, AuthorityError> {
    Ok(vec![
        native_input("module/source.go", source.to_vec())?,
        native_input("go.mod", modfile.to_vec())?,
        native_input("build-tags", tags.as_bytes().to_vec())?,
        native_input(
            "go",
            backend_compile::native_executable_evidence(Path::new(go)),
        )?,
        native_input(
            "authority-helper",
            backend_compile::native_executable_evidence(Path::new(helper)),
        )?,
        native_input("vendor/optional", b"absent".to_vec())?,
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
