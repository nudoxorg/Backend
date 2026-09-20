//! C and C++ native authority adapter.
#![forbid(unsafe_code)]

use backend_compile::{
    Authority, AuthorityError, AuthorityIdentity, DiscoverySnapshot, Extraction, Input, InputKind,
    InputManifest, NativeRequestInput, NativeTemplate, ProcessLimits, ProtocolDescriptor,
    SessionKey, SupervisedCommand, default_native_limits, extract_native_checked, native_input,
    native_semantic_evidence, native_semantic_input, typed_of,
};
use std::path::Path;

const LANGUAGE: &str = "clang";

/// Builds the zero-toolchain local C-family syntax frontend.
///
/// # Errors
/// Returns an error when the embedded grammar query cannot be admitted.
pub fn syntax_frontend() -> Result<backend_compile::SyntaxFrontend, backend_compile::SyntaxError> {
    backend_compile::SyntaxFrontend::new(
        backend_compile::SourceLanguage::Clang,
        b"tree-sitter-cpp-0.23.4/tags-v1",
        vec![backend_compile::GrammarVariant::new(
            &["c", "h", "cc", "cpp", "cxx", "hh", "hpp", "hxx", "m", "mm"],
            tree_sitter_cpp::LANGUAGE.into(),
            tree_sitter_cpp::TAGS_QUERY,
        )?],
    )
}

/// C/C++ authority with explicit compilation command identity.
pub struct ClangFrontend {
    source: Vec<u8>,
    toolchain: String,
    command: String,
    helper: Option<String>,
    template: Option<NativeTemplate>,
}

impl ClangFrontend {
    /// Creates a manifest-only configuration.
    ///
    /// Clang itself does not emit the backend authority payload. Call
    /// [`Self::with_helper`] to select an explicit adapter executable.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn new(
        source: Vec<u8>,
        toolchain: impl Into<String>,
        command: impl Into<String>,
    ) -> Result<Self, AuthorityError> {
        let toolchain = toolchain.into();
        let command = command.into();
        validate_absolute(&toolchain, "clang toolchain")?;
        Ok(Self {
            source,
            toolchain,
            command,
            helper: None,
            template: None,
        })
    }

    /// Creates a cold authority using an explicit absolute helper.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn with_helper(
        source: Vec<u8>,
        toolchain: impl Into<String>,
        helper: impl Into<String>,
        command: impl Into<String>,
    ) -> Result<Self, AuthorityError> {
        Self::with_mode_and_limits(
            source,
            toolchain,
            helper,
            command,
            false,
            default_native_limits()?,
        )
    }

    /// Creates a persistent C/C++ authority using an explicit helper.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn with_persistent_helper(
        source: Vec<u8>,
        toolchain: impl Into<String>,
        helper: impl Into<String>,
        command: impl Into<String>,
    ) -> Result<Self, AuthorityError> {
        Self::with_mode_and_limits(
            source,
            toolchain,
            helper,
            command,
            true,
            default_native_limits()?,
        )
    }

    /// Creates a cold authority with an explicit process budget.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn with_helper_and_limits(
        source: Vec<u8>,
        toolchain: impl Into<String>,
        helper: impl Into<String>,
        command: impl Into<String>,
        limits: ProcessLimits,
    ) -> Result<Self, AuthorityError> {
        Self::with_mode_and_limits(source, toolchain, helper, command, false, limits)
    }

    /// Creates a persistent C/C++ authority with an explicit process budget.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn with_persistent_helper_and_limits(
        source: Vec<u8>,
        toolchain: impl Into<String>,
        helper: impl Into<String>,
        command: impl Into<String>,
        limits: ProcessLimits,
    ) -> Result<Self, AuthorityError> {
        Self::with_mode_and_limits(source, toolchain, helper, command, true, limits)
    }

    fn with_mode_and_limits(
        source: Vec<u8>,
        toolchain: impl Into<String>,
        helper: impl Into<String>,
        command: impl Into<String>,
        persistent: bool,
        limits: ProcessLimits,
    ) -> Result<Self, AuthorityError> {
        let toolchain = toolchain.into();
        let helper = helper.into();
        let command = command.into();
        validate_absolute(&toolchain, "clang toolchain")?;
        validate_absolute(&helper, "clang authority helper")?;
        let template = NativeTemplate::new(
            LANGUAGE,
            &helper,
            &toolchain,
            backend_compile::native_executable_id(Path::new(&toolchain)),
            if persistent {
                ProtocolDescriptor::persistent()
            } else {
                ProtocolDescriptor::cold()
            },
            limits,
        )?;
        Ok(Self {
            source,
            toolchain,
            command,
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

    /// Returns the checked helper command, when a helper was configured.
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
        let helper = self.helper_input()?;
        InputManifest::new(vec![
            Input::new(InputKind::Source, "translation-unit", &self.source).map_err(discovery)?,
            Input::new(
                InputKind::Toolchain,
                "clang",
                &backend_compile::native_executable_evidence(Path::new(&self.toolchain)),
            )
            .map_err(discovery)?,
            Input::new(
                InputKind::Configuration,
                "compile-command",
                self.command.as_bytes(),
            )
            .map_err(discovery)?,
            Input::absent(InputKind::NegativeDependency, "missing/optional-header")
                .map_err(discovery)?,
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

    fn helper_input(&self) -> Result<Input, AuthorityError> {
        match &self.helper {
            Some(helper) => Input::new(
                InputKind::Dependency,
                "authority-helper",
                &backend_compile::native_executable_evidence(Path::new(helper)),
            )
            .map_err(discovery),
            None => Input::absent(InputKind::Dependency, "authority-helper").map_err(discovery),
        }
    }
}

impl Authority for ClangFrontend {
    fn identity(&self) -> AuthorityIdentity {
        AuthorityIdentity {
            producer: typed_of(b"backend-frontend-clang-v3"),
            toolchain: backend_compile::native_executable_id(Path::new(&self.toolchain)),
            contract: typed_of(b"native-fact-envelope-v1/clang"),
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
            &self.toolchain,
            self.helper.as_deref().unwrap_or("unsupported"),
            &self.command,
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
    toolchain: &str,
    helper: &str,
    command: &str,
) -> Result<Vec<NativeRequestInput>, AuthorityError> {
    Ok(vec![
        native_input("translation-unit", source.to_vec())?,
        native_input("compile-command", command.as_bytes().to_vec())?,
        native_input(
            "clang",
            backend_compile::native_executable_evidence(Path::new(toolchain)),
        )?,
        native_input(
            "authority-helper",
            backend_compile::native_executable_evidence(Path::new(helper)),
        )?,
        native_input("missing/optional-header", b"absent".to_vec())?,
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
