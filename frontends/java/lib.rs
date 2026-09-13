//! Java `javac`/doclet native authority adapter.
#![forbid(unsafe_code)]

use backend_compile::{
    Authority, AuthorityError, AuthorityIdentity, DiscoverySnapshot, Extraction, FactKeySchema,
    FactKind, FactRecord, FactValueSchema, Input, InputKind, InputManifest, NativeRecord,
    NativeRecordKind, NativeRequestInput, NativeSemanticAdapter, NativeSemanticRequest,
    NativeTemplate, ProcessLimits, ProtocolDescriptor, SessionKey, SupervisedCommand,
    default_native_limits, extract_native_with_adapter, native_helper_evidence, native_input,
    native_semantic_evidence, native_semantic_input, typed_of,
};
use sha2::{Digest, Sha256};
use std::{fmt, path::Path};

pub mod central;
pub mod harness;
pub mod jar;
pub mod purl;
pub mod repo;

pub use backend_compile::JavaRelease;

const LANGUAGE: &str = "java";
const PAYLOAD_VERSION: &str = "java-semantic-v1";

struct JavaSemanticAdapter;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JavaPayload<'a> {
    Declaration {
        kind: &'a str,
        owner: &'a str,
        signature: &'a str,
        documentation: &'a str,
    },
    Type {
        owner: &'a str,
        signature: &'a str,
    },
    Reference {
        owner: &'a str,
        target: &'a str,
        start: u32,
        end: u32,
    },
    Diagnostic {
        severity: &'a str,
        code: &'a str,
        start: u32,
        end: u32,
        message: &'a str,
    },
    Package {
        name: &'a str,
        state: &'a str,
    },
}

#[derive(Clone, Copy)]
struct ParsedJavaRecord<'a> {
    kind: NativeRecordKind,
    key: &'a str,
    bytes: &'a [u8],
    payload: JavaPayload<'a>,
}
#[derive(Clone, Copy)]
struct AdmittedJavaRecord<'a>(ParsedJavaRecord<'a>);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JavaSemanticError {
    NonUtf8,
    WrongVersion,
    WrongShape,
    EmptyField,
    InvalidNumber,
    InvalidSpan,
    InvalidSeverity,
    InvalidDependencyState,
    InvalidTypeSignature,
}

impl fmt::Display for JavaSemanticError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NonUtf8 => "Java semantic payload is not UTF-8",
            Self::WrongVersion => "Java semantic payload has the wrong schema version",
            Self::WrongShape => "Java semantic payload has the wrong field shape",
            Self::EmptyField => "Java semantic payload has an empty required field",
            Self::InvalidNumber => "Java semantic payload has an invalid numeric field",
            Self::InvalidSpan => "Java semantic payload has an invalid source span",
            Self::InvalidSeverity => "Java diagnostic has an invalid severity",
            Self::InvalidDependencyState => "Java package observation has an invalid state",
            Self::InvalidTypeSignature => "Java semantic type signature is invalid",
        })
    }
}

impl std::error::Error for JavaSemanticError {}

impl NativeSemanticAdapter for JavaSemanticAdapter {
    type Parsed<'record> = ParsedJavaRecord<'record>;
    type Admitted<'record> = AdmittedJavaRecord<'record>;
    type Error = JavaSemanticError;

    fn parse<'record>(
        &'record self,
        record: &'record NativeRecord,
    ) -> Result<Self::Parsed<'record>, Self::Error> {
        let value = std::str::from_utf8(record.value()).map_err(|_| JavaSemanticError::NonUtf8)?;
        let fields = value.split('\0').collect::<Vec<_>>();
        if fields.first().copied() != Some(PAYLOAD_VERSION) {
            return Err(JavaSemanticError::WrongVersion);
        }
        let payload = match (record.kind(), fields.as_slice()) {
            (NativeRecordKind::Declaration, [_, kind, owner, signature, documentation]) => {
                JavaPayload::Declaration {
                    kind,
                    owner,
                    signature,
                    documentation,
                }
            }
            (NativeRecordKind::Type, [_, owner, signature]) => {
                JavaPayload::Type { owner, signature }
            }
            (NativeRecordKind::Edge, [_, owner, target, start, end]) => JavaPayload::Reference {
                owner,
                target,
                start: start
                    .parse()
                    .map_err(|_| JavaSemanticError::InvalidNumber)?,
                end: end.parse().map_err(|_| JavaSemanticError::InvalidNumber)?,
            },
            (NativeRecordKind::Diagnostic, [_, severity, code, start, end, message]) => {
                JavaPayload::Diagnostic {
                    severity,
                    code,
                    start: start
                        .parse()
                        .map_err(|_| JavaSemanticError::InvalidNumber)?,
                    end: end.parse().map_err(|_| JavaSemanticError::InvalidNumber)?,
                    message,
                }
            }
            (
                NativeRecordKind::Dependency | NativeRecordKind::NegativeDependency,
                [_, "package", name, state],
            ) => JavaPayload::Package { name, state },
            _ => return Err(JavaSemanticError::WrongShape),
        };
        Ok(ParsedJavaRecord {
            kind: record.kind(),
            key: record.key(),
            bytes: record.value(),
            payload,
        })
    }

    fn admit<'record>(
        &'record self,
        parsed: Self::Parsed<'record>,
    ) -> Result<Self::Admitted<'record>, Self::Error> {
        match parsed.payload {
            JavaPayload::Declaration {
                kind,
                owner,
                signature,
                documentation: _,
            } => {
                java_required(&[kind, owner, signature])?;
                validate_java_type(signature)?;
            }
            JavaPayload::Type { owner, signature } => {
                java_required(&[owner, signature])?;
                validate_java_type(signature)?;
            }
            JavaPayload::Reference {
                owner,
                target,
                start,
                end,
            } => {
                java_required(&[owner, target])?;
                java_span(start, end)?;
            }
            JavaPayload::Diagnostic {
                severity,
                code,
                start,
                end,
                message,
            } => {
                java_required(&[code, message])?;
                if !matches!(
                    severity,
                    "note" | "warning" | "mandatory-warning" | "error" | "other"
                ) {
                    return Err(JavaSemanticError::InvalidSeverity);
                }
                java_span(start, end)?;
            }
            JavaPayload::Package { name, state } => {
                java_required(&[name])?;
                let expected = match parsed.kind {
                    NativeRecordKind::Dependency => "present",
                    NativeRecordKind::NegativeDependency => "absent",
                    _ => return Err(JavaSemanticError::WrongShape),
                };
                if state != expected {
                    return Err(JavaSemanticError::InvalidDependencyState);
                }
            }
        }
        Ok(AdmittedJavaRecord(parsed))
    }

    fn lower<'record>(
        &'record self,
        admitted: Self::Admitted<'record>,
    ) -> FactRecord<FactKeySchema, FactValueSchema> {
        let record = admitted.0;
        FactRecord::new(
            java_fact_kind(record.kind),
            record.key.as_bytes().to_vec(),
            record.bytes.to_vec(),
        )
    }
}

fn java_required(fields: &[&str]) -> Result<(), JavaSemanticError> {
    if fields.iter().any(|field| field.is_empty()) {
        Err(JavaSemanticError::EmptyField)
    } else {
        Ok(())
    }
}
fn java_span(start: u32, end: u32) -> Result<(), JavaSemanticError> {
    if start <= end {
        Ok(())
    } else {
        Err(JavaSemanticError::InvalidSpan)
    }
}
fn validate_java_type(signature: &str) -> Result<(), JavaSemanticError> {
    let mut stack = Vec::new();
    for byte in signature.bytes() {
        match byte {
            b'<' | b'[' | b'(' => {
                if stack.len() == 64 {
                    return Err(JavaSemanticError::InvalidTypeSignature);
                }
                stack.push(byte);
            }
            b'>' if stack.pop() != Some(b'<') => {
                return Err(JavaSemanticError::InvalidTypeSignature);
            }
            b']' if stack.pop() != Some(b'[') => {
                return Err(JavaSemanticError::InvalidTypeSignature);
            }
            b')' if stack.pop() != Some(b'(') => {
                return Err(JavaSemanticError::InvalidTypeSignature);
            }
            0..=31 | 127 => return Err(JavaSemanticError::InvalidTypeSignature),
            _ => {}
        }
    }
    if stack.is_empty() {
        Ok(())
    } else {
        Err(JavaSemanticError::InvalidTypeSignature)
    }
}

const fn java_fact_kind(kind: NativeRecordKind) -> FactKind {
    match kind {
        NativeRecordKind::Declaration => FactKind::Declaration,
        NativeRecordKind::Type => FactKind::Type,
        NativeRecordKind::Edge => FactKind::Edge,
        NativeRecordKind::Diagnostic => FactKind::Diagnostic,
        NativeRecordKind::Dependency => FactKind::Dependency,
        NativeRecordKind::NegativeDependency => FactKind::NegativeDependency,
    }
}

/// Builds the zero-toolchain local Java syntax frontend.
///
/// This structural baseline never claims native semantic authority.
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
    release: JavaRelease,
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
        let release = release.into();
        let release = parse_release(&release)?;
        validate_absolute(&jdk, "JDK executable")?;
        Ok(Self {
            source,
            jdk,
            classpath,
            release,
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

    /// Creates a cold Java authority using the bundled javac/doclet bridge.
    ///
    /// The bridge is source distributed: `helper/backend-java-authority`
    /// launches the checked JDK beside the selected `javac` and executes the
    /// BCQ/BCN semantic helper. A caller that has a separately published
    /// helper can continue to use [`Self::with_helper`].
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn with_bundled_helper(
        source: Vec<u8>,
        jdk: impl Into<String>,
        classpath: Vec<u8>,
        release: impl Into<String>,
    ) -> Result<Self, AuthorityError> {
        let helper = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("helper/backend-java-authority")
            .to_string_lossy()
            .into_owned();
        Self::with_helper(source, jdk, helper, classpath, release)
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
        let release = parse_release(&release)?;
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
                &native_helper_evidence(Path::new(path)),
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
            Input::new(
                InputKind::Dependency,
                "classpath-artifacts",
                &classpath_evidence(&self.classpath),
            )
            .map_err(discovery)?,
            Input::new(
                InputKind::Configuration,
                "release",
                self.release.name().as_bytes(),
            )
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
            self.release,
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
            &JavaSemanticAdapter,
        )
    }
}

fn request_inputs(
    source: &[u8],
    jdk: &str,
    helper: &str,
    classpath: &[u8],
    release: JavaRelease,
) -> Result<Vec<NativeRequestInput>, AuthorityError> {
    Ok(vec![
        native_input("src/Main.java", source.to_vec())?,
        native_input("classpath", classpath.to_vec())?,
        native_input("classpath-artifacts", classpath_evidence(classpath))?,
        native_input("release", release.name().as_bytes().to_vec())?,
        native_input(
            "jdk",
            backend_compile::native_executable_evidence(Path::new(jdk)),
        )?,
        native_input(
            "authority-helper",
            native_helper_evidence(Path::new(helper)),
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

/// Binds every classpath entry to the discovery manifest by path and content.
///
/// The compiler receives the original path list, while this compact sidecar
/// proves that a JAR or module directory did not change between discovery and
/// extraction. Missing entries stay explicit so an absent optional classpath
/// cannot become an ambient host dependency.
fn classpath_evidence(classpath: &[u8]) -> Vec<u8> {
    const LIMIT: usize = 192 * 1024;
    let text = String::from_utf8_lossy(classpath);
    let separator = if cfg!(windows) { ';' } else { ':' };
    let mut evidence = b"java-classpath-v1\0".to_vec();
    for entry in text.split(separator).filter(|entry| !entry.is_empty()) {
        let path = Path::new(entry);
        evidence.extend_from_slice(entry.as_bytes());
        evidence.push(0);
        match std::fs::read(path) {
            Ok(bytes) => {
                evidence.push(1);
                evidence.extend_from_slice(&Sha256::digest(bytes));
            }
            Err(_) => evidence.extend_from_slice(b"missing"),
        }
        evidence.push(0);
        if evidence.len() >= LIMIT {
            evidence.truncate(LIMIT);
            break;
        }
    }
    evidence
}

fn parse_release(release: &str) -> Result<JavaRelease, AuthorityError> {
    JavaRelease::try_from(release).map_err(discovery)
}

fn discovery<E: fmt::Display>(error: E) -> AuthorityError {
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

#[cfg(test)]
mod semantic_tests {
    use super::*;
    use std::error::Error;

    fn record(
        kind: NativeRecordKind,
        key: &str,
        fields: &[&str],
    ) -> Result<NativeRecord, backend_compile::NativeProtocolError> {
        NativeRecord::new(kind, key, fields.join("\0").into_bytes())
    }

    #[test]
    fn admits_javac_declarations_recursive_types_references_docs_diagnostics_and_packages()
    -> Result<(), Box<dyn Error>> {
        let cases = [
            record(
                NativeRecordKind::Declaration,
                "demo.Node<T>",
                &[
                    PAYLOAD_VERSION,
                    "class",
                    "demo",
                    "demo.Node<demo.Node<T[]>>",
                    "Recursive node.",
                ],
            )?,
            record(
                NativeRecordKind::Type,
                "demo.Node<T>",
                &[PAYLOAD_VERSION, "demo.Node<T>", "demo.Node<demo.Node<T[]>>"],
            )?,
            record(
                NativeRecordKind::Edge,
                "demo.App.run()->demo.Factory.make()@8:12",
                &[
                    PAYLOAD_VERSION,
                    "demo.App.run()",
                    "demo.Factory.make()",
                    "8",
                    "12",
                ],
            )?,
            record(
                NativeRecordKind::Diagnostic,
                "compiler.warn.deprecation@8:12",
                &[
                    PAYLOAD_VERSION,
                    "warning",
                    "compiler.warn.deprecation",
                    "8",
                    "12",
                    "deprecated member",
                ],
            )?,
            record(
                NativeRecordKind::Dependency,
                "package:java.base",
                &[PAYLOAD_VERSION, "package", "java.base", "present"],
            )?,
            record(
                NativeRecordKind::NegativeDependency,
                "package:optional",
                &[PAYLOAD_VERSION, "package", "optional", "absent"],
            )?,
        ];
        for native in &cases {
            let parsed = JavaSemanticAdapter.parse(native)?;
            let admitted = JavaSemanticAdapter.admit(parsed)?;
            let lowered = JavaSemanticAdapter.lower(admitted);
            assert_eq!(lowered.value(), native.value());
        }
        Ok(())
    }

    #[test]
    fn rejects_reversed_reference_span_and_false_package_claim() -> Result<(), Box<dyn Error>> {
        let reference = record(
            NativeRecordKind::Edge,
            "bad",
            &[
                PAYLOAD_VERSION,
                "demo.App.run()",
                "demo.Target.call()",
                "12",
                "8",
            ],
        )?;
        assert!(matches!(
            JavaSemanticAdapter.admit(JavaSemanticAdapter.parse(&reference)?),
            Err(JavaSemanticError::InvalidSpan)
        ));
        let package = record(
            NativeRecordKind::Dependency,
            "package:x",
            &[PAYLOAD_VERSION, "package", "x", "absent"],
        )?;
        assert!(matches!(
            JavaSemanticAdapter.admit(JavaSemanticAdapter.parse(&package)?),
            Err(JavaSemanticError::InvalidDependencyState)
        ));
        Ok(())
    }
}
