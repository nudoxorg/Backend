//! Python Ruff/pyrefly native authority adapter.
#![forbid(unsafe_code)]

use backend_compile::{
    Authority, AuthorityError, AuthorityIdentity, DiscoverySnapshot, Extraction, Input, InputKind,
    InputManifest, NativeRequestInput, NativeTemplate, ProcessLimits, ProtocolDescriptor,
    SessionKey, SupervisedCommand, default_native_limits, extract_native_checked, native_input,
    native_semantic_evidence, native_semantic_input, typed_of,
};
use std::path::Path;

const LANGUAGE: &str = "python";

/// Builds the zero-toolchain local Python syntax frontend.
///
/// # Errors
/// Returns an error when the embedded grammar query cannot be admitted.
pub fn syntax_frontend() -> Result<backend_compile::SyntaxFrontend, backend_compile::SyntaxError> {
    backend_compile::SyntaxFrontend::new(
        backend_compile::SourceLanguage::Python,
        b"tree-sitter-python-0.25.0/tags-v1",
        vec![backend_compile::GrammarVariant::new(
            &["py", "pyi"],
            tree_sitter_python::LANGUAGE.into(),
            tree_sitter_python::TAGS_QUERY,
        )?],
    )
}

/// Python authority configuration with explicit interpreter/probe identity.
pub struct PythonFrontend {
    source: Vec<u8>,
    python: String,
    pyrefly: String,
    profile: String,
    config: Vec<u8>,
    template: Option<NativeTemplate>,
}

impl PythonFrontend {
    /// Creates a manifest-only Python configuration.
    ///
    /// Python tooling does not emit the backend authority payload directly.
    /// Call [`Self::with_helper`] to select an explicit protocol adapter;
    /// interpreter stdout is never interpreted as a backend payload.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn new(
        source: Vec<u8>,
        python: impl Into<String>,
        pyrefly: impl Into<String>,
        profile: impl Into<String>,
        config: Vec<u8>,
    ) -> Result<Self, AuthorityError> {
        let python = python.into();
        let pyrefly = pyrefly.into();
        validate_absolute(&python, "Python executable")?;
        validate_absolute(&pyrefly, "pyrefly authority helper")?;
        Ok(Self {
            source,
            python,
            pyrefly,
            profile: profile.into(),
            config,
            template: None,
        })
    }

    /// Creates a cold Python authority using an explicit absolute helper.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn with_helper(
        source: Vec<u8>,
        python: impl Into<String>,
        pyrefly: impl Into<String>,
        profile: impl Into<String>,
        config: Vec<u8>,
    ) -> Result<Self, AuthorityError> {
        Self::with_mode_and_limits(
            source,
            python,
            pyrefly,
            profile,
            config,
            false,
            default_native_limits()?,
        )
    }

    /// Creates a persistent Python authority using an explicit helper.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn with_persistent_helper(
        source: Vec<u8>,
        python: impl Into<String>,
        pyrefly: impl Into<String>,
        profile: impl Into<String>,
        config: Vec<u8>,
    ) -> Result<Self, AuthorityError> {
        Self::with_mode_and_limits(
            source,
            python,
            pyrefly,
            profile,
            config,
            true,
            default_native_limits()?,
        )
    }

    /// Creates a cold Python authority with an explicit process budget.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn with_helper_and_limits(
        source: Vec<u8>,
        python: impl Into<String>,
        pyrefly: impl Into<String>,
        profile: impl Into<String>,
        config: Vec<u8>,
        limits: ProcessLimits,
    ) -> Result<Self, AuthorityError> {
        Self::with_mode_and_limits(source, python, pyrefly, profile, config, false, limits)
    }

    /// Creates a persistent Python authority with an explicit process budget.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn with_persistent_helper_and_limits(
        source: Vec<u8>,
        python: impl Into<String>,
        pyrefly: impl Into<String>,
        profile: impl Into<String>,
        config: Vec<u8>,
        limits: ProcessLimits,
    ) -> Result<Self, AuthorityError> {
        Self::with_mode_and_limits(source, python, pyrefly, profile, config, true, limits)
    }

    fn with_mode_and_limits(
        source: Vec<u8>,
        python: impl Into<String>,
        pyrefly: impl Into<String>,
        profile: impl Into<String>,
        config: Vec<u8>,
        persistent: bool,
        limits: ProcessLimits,
    ) -> Result<Self, AuthorityError> {
        let python = python.into();
        let pyrefly = pyrefly.into();
        let profile = profile.into();
        validate_absolute(&python, "Python executable")?;
        validate_absolute(&pyrefly, "pyrefly authority helper")?;
        let template = NativeTemplate::new(
            LANGUAGE,
            &pyrefly,
            &python,
            backend_compile::native_executable_id(Path::new(&python)),
            if persistent {
                ProtocolDescriptor::persistent()
            } else {
                ProtocolDescriptor::cold()
            },
            limits,
        )?;
        Ok(Self {
            source,
            python,
            pyrefly,
            profile,
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

    /// Returns the configured pyrefly helper path.
    #[must_use]
    pub fn helper(&self) -> &Path {
        Path::new(&self.pyrefly)
    }

    fn manifest(&self) -> Result<InputManifest, AuthorityError> {
        InputManifest::new(vec![
            Input::new(InputKind::Source, "module.py", &self.source).map_err(discovery)?,
            Input::new(
                InputKind::Toolchain,
                "python",
                &backend_compile::native_executable_evidence(Path::new(&self.python)),
            )
            .map_err(discovery)?,
            Input::new(
                InputKind::Toolchain,
                "pyrefly",
                &backend_compile::native_executable_evidence(Path::new(&self.pyrefly)),
            )
            .map_err(discovery)?,
            Input::new(
                InputKind::Configuration,
                "python-profile",
                self.profile.as_bytes(),
            )
            .map_err(discovery)?,
            Input::new(InputKind::Configuration, "pyrefly-config", &self.config)
                .map_err(discovery)?,
            Input::absent(InputKind::NegativeDependency, "imports/optional").map_err(discovery)?,
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

impl Authority for PythonFrontend {
    fn identity(&self) -> AuthorityIdentity {
        AuthorityIdentity {
            producer: typed_of(b"backend-frontend-python-v3"),
            toolchain: backend_compile::native_executable_id(Path::new(&self.python)),
            contract: typed_of(b"native-fact-envelope-v1/python"),
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
        if self.template.is_none()
            && (!Path::new(&self.python).exists() || !Path::new(&self.pyrefly).exists())
        {
            return backend_compile::unavailable_extraction(self.identity(), snapshot);
        }
        let fields = request_inputs(
            &self.source,
            &self.python,
            &self.pyrefly,
            &self.profile,
            &self.config,
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
    python: &str,
    pyrefly: &str,
    profile: &str,
    config: &[u8],
) -> Result<Vec<NativeRequestInput>, AuthorityError> {
    Ok(vec![
        native_input("module.py", source.to_vec())?,
        native_input("python-profile", profile.as_bytes().to_vec())?,
        native_input("pyrefly-config", config.to_vec())?,
        native_input(
            "python",
            backend_compile::native_executable_evidence(Path::new(python)),
        )?,
        native_input(
            "pyrefly",
            backend_compile::native_executable_evidence(Path::new(pyrefly)),
        )?,
        native_input("imports/optional", b"absent".to_vec())?,
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
