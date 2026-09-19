//! Java `javac`/doclet native authority adapter.
#![forbid(unsafe_code)]

use backend_compile::{
    Authority, AuthorityError, AuthorityIdentity, DiscoverySnapshot, Extraction, Input, InputKind,
    InputManifest, NativeRequestInput, NativeTemplate, ProcessLimits, ProtocolDescriptor,
    SessionKey, SupervisedCommand, default_native_limits, extract_native_checked, native_input,
    native_semantic_evidence, native_semantic_input, typed_of,
};
use std::path::Path;

const LANGUAGE: &str = "java";

/// Builds the zero-toolchain local Java syntax frontend.
///
/// # Errors
/// Returns an error when the embedded grammar query cannot be admitted.
pub fn syntax_frontend() -> Result<backend_compile::SyntaxFrontend, backend_compile::SyntaxError> {
    backend_compile::SyntaxFrontend::new(
        backend_compile::SourceLanguage::Java,
        b"tree-sitter-java-0.23.5/tags-v1",
        vec![backend_compile::GrammarVariant::new(
            &["java"],
            tree_sitter_java::LANGUAGE.into(),
            tree_sitter_java::TAGS_QUERY,
        )?],
    )
}

/// Java authority configuration with JDK, classpath, and source inputs.
pub struct JavaFrontend {
    source: Vec<u8>,
    jdk: String,
    classpath: Vec<u8>,
    release: String,
    helper: Option<String>,
    template: Option<NativeTemplate>,
}

impl JavaFrontend {
    /// Creates a manifest-only Java configuration.
    ///
    /// `javac` does not emit the backend authority payload. Call
    /// [`Self::with_helper`] to select an explicit doclet adapter.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn new(
        source: Vec<u8>,
        jdk: impl Into<String>,
        classpath: Vec<u8>,
        release: impl Into<String>,
    ) -> Result<Self, AuthorityError> {
        let jdk = jdk.into();
        validate_absolute(&jdk, "JDK executable")?;
        Ok(Self {
            source,
            jdk,
            classpath,
            release: release.into(),
            helper: None,
            template: None,
        })
    }

    /// Creates a cold Java authority using an explicit absolute helper.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn with_helper(
        source: Vec<u8>,
        jdk: impl Into<String>,
        helper: impl Into<String>,
        classpath: Vec<u8>,
        release: impl Into<String>,
    ) -> Result<Self, AuthorityError> {
        Self::with_mode_and_limits(
            source,
            jdk,
            helper,
            classpath,
            release,
            false,
            default_native_limits()?,
        )
    }

    /// Creates a persistent Java authority using an explicit helper.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn with_persistent_helper(
        source: Vec<u8>,
        jdk: impl Into<String>,
        helper: impl Into<String>,
        classpath: Vec<u8>,
        release: impl Into<String>,
    ) -> Result<Self, AuthorityError> {
        Self::with_mode_and_limits(
            source,
            jdk,
            helper,
            classpath,
            release,
            true,
            default_native_limits()?,
        )
    }

    /// Creates a cold Java authority with an explicit process budget.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn with_helper_and_limits(
        source: Vec<u8>,
        jdk: impl Into<String>,
        helper: impl Into<String>,
        classpath: Vec<u8>,
        release: impl Into<String>,
        limits: ProcessLimits,
    ) -> Result<Self, AuthorityError> {
        Self::with_mode_and_limits(source, jdk, helper, classpath, release, false, limits)
    }

    /// Creates a persistent Java authority with an explicit process budget.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn with_persistent_helper_and_limits(
        source: Vec<u8>,
        jdk: impl Into<String>,
        helper: impl Into<String>,
        classpath: Vec<u8>,
        release: impl Into<String>,
        limits: ProcessLimits,
    ) -> Result<Self, AuthorityError> {
        Self::with_mode_and_limits(source, jdk, helper, classpath, release, true, limits)
    }

    fn with_mode_and_limits(
        source: Vec<u8>,
        jdk: impl Into<String>,
        helper: impl Into<String>,
        classpath: Vec<u8>,
        release: impl Into<String>,
        persistent: bool,
        limits: ProcessLimits,
    ) -> Result<Self, AuthorityError> {
        let jdk = jdk.into();
        let helper = helper.into();
        let release = release.into();
        validate_absolute(&jdk, "JDK executable")?;
        validate_absolute(&helper, "Java authority helper")?;
        let template = NativeTemplate::new(
            LANGUAGE,
            &helper,
            &jdk,
            backend_compile::native_executable_id(Path::new(&jdk)),
            if persistent {
                ProtocolDescriptor::persistent()
            } else {
                ProtocolDescriptor::cold()
            },
            limits,
        )?;
        Ok(Self {
            source,
            jdk,
            classpath,
            release,
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
            Input::new(InputKind::Source, "src/Main.java", &self.source).map_err(discovery)?,
            Input::new(
                InputKind::Toolchain,
                "jdk",
                &backend_compile::native_executable_evidence(Path::new(&self.jdk)),
            )
            .map_err(discovery)?,
            Input::new(InputKind::Dependency, "classpath", &self.classpath).map_err(discovery)?,
            Input::new(InputKind::Configuration, "release", self.release.as_bytes())
                .map_err(discovery)?,
            Input::absent(InputKind::NegativeDependency, "classpath/optional")
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
}

impl Authority for JavaFrontend {
    fn identity(&self) -> AuthorityIdentity {
        AuthorityIdentity {
            producer: typed_of(b"backend-frontend-java-v3"),
            toolchain: backend_compile::native_executable_id(Path::new(&self.jdk)),
            contract: typed_of(b"native-fact-envelope-v1/java"),
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
            &self.jdk,
            self.helper.as_deref().unwrap_or("unsupported"),
            &self.classpath,
            &self.release,
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
    jdk: &str,
    helper: &str,
    classpath: &[u8],
    release: &str,
) -> Result<Vec<NativeRequestInput>, AuthorityError> {
    Ok(vec![
        native_input("src/Main.java", source.to_vec())?,
        native_input("classpath", classpath.to_vec())?,
        native_input("release", release.as_bytes().to_vec())?,
        native_input(
            "jdk",
            backend_compile::native_executable_evidence(Path::new(jdk)),
        )?,
        native_input(
            "authority-helper",
            backend_compile::native_executable_evidence(Path::new(helper)),
        )?,
        native_input("classpath/optional", b"absent".to_vec())?,
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
