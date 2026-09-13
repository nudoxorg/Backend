//! Python Ruff/pyrefly native authority adapter.
#![forbid(unsafe_code)]

mod checker;
mod facts;

pub use checker::{
    CheckerError, CheckerReport, Diagnostic, ImportResolution, Inference, InferenceSite,
    InferredType, Pyrefly, PyreflyExecutableError, SymbolOutcome, SymbolResolution, WorkerPanic,
    parse_revealed_type,
};
pub use facts::{
    Annotation, AnnotationFact, AnnotationPosition, AnnotationSyntaxKind, ClassForm, Confidence,
    DeclarationFact, DeclarationKind, DocstringFact, ExtractionError, LiteralValue, ModuleFacts,
    OccurrenceFact, OccurrenceKind, ParameterFact, ParameterKind, ReceiverKind, RejectedSyntax,
    RuffDeclaration, RuffDeclarationKind, RuffModule, Span, TypeReason, extract, with_module,
};

use backend_compile::{
    Authority, AuthorityError, AuthorityIdentity, DiscoverySnapshot, Extraction, FactKeySchema,
    FactKind, FactRecord, FactValueSchema, Input, InputKind, InputManifest, NativeRecord,
    NativeRecordKind, NativeRequestInput, NativeSemanticAdapter, NativeSemanticRequest,
    NativeTemplate, ProcessLimits, ProtocolDescriptor, PythonVersion, SessionKey,
    SupervisedCommand, default_native_limits, extract_native_with_adapter, native_input,
    native_semantic_evidence, native_semantic_input, typed_of,
};
use serde::Deserialize;
use std::fmt;
use std::path::Path;

const LANGUAGE: &str = "python";

struct PythonSemanticAdapter;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceSpan {
    start: u32,
    end: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum DeclarationShape {
    Module,
    Class,
    Function,
    Field,
    Constant,
    Alias,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum PayloadClassForm {
    Plain,
    Dataclass,
    Protocol,
    TypedDict,
    Enum,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ParameterShape {
    PositionalOnly,
    PositionalOrKeyword,
    VarArgs,
    KeywordOnly,
    KwArgs,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ParameterPayload<'a> {
    name: &'a str,
    kind: ParameterShape,
    span: SourceSpan,
    annotation: Option<&'a str>,
    has_default: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeclarationPayload<'a> {
    owner: &'a str,
    name: &'a str,
    kind: DeclarationShape,
    span: SourceSpan,
    signature: &'a str,
    documentation: Option<&'a str>,
    class_form: Option<PayloadClassForm>,
    parameters: Vec<ParameterPayload<'a>>,
    annotations: Vec<&'a str>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum PayloadInferenceSite {
    ModuleBinding,
    Parameter,
    Return,
    Field,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TypePayload<'a> {
    owner: &'a str,
    site: PayloadInferenceSite,
    span: SourceSpan,
    inferred: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Resolution {
    Local,
    Foreign,
    Unresolved,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EdgePayload<'a> {
    owner: &'a str,
    target: &'a str,
    span: SourceSpan,
    resolution: Resolution,
    module: Option<&'a str>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Severity {
    Error,
    Warning,
    Information,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DiagnosticPayload<'a> {
    severity: Severity,
    code: &'a str,
    span: SourceSpan,
    message: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportPayload<'a> {
    binding: &'a str,
    module: &'a str,
    span: SourceSpan,
}

enum PythonPayload<'record> {
    Declaration(DeclarationPayload<'record>),
    Type(TypePayload<'record>),
    Edge(EdgePayload<'record>),
    Diagnostic(DiagnosticPayload<'record>),
    Import(ImportPayload<'record>),
}

struct ParsedPythonRecord<'record> {
    record: &'record NativeRecord,
    payload: PythonPayload<'record>,
}

struct AdmittedPythonRecord<'record>(&'record NativeRecord);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PythonSemanticError {
    MalformedJson,
    EmptyIdentity,
    InvalidSpan,
    UnusableType,
    InvalidResolution,
}

impl fmt::Display for PythonSemanticError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::MalformedJson => "Python semantic payload is not canonical structured JSON",
            Self::EmptyIdentity => "Python semantic payload contains an empty identity",
            Self::InvalidSpan => "Python semantic payload contains an invalid source span",
            Self::UnusableType => "Python type authority returned Any or Unknown",
            Self::InvalidResolution => "Python foreign resolution has no module",
        })
    }
}

impl std::error::Error for PythonSemanticError {}

impl NativeSemanticAdapter for PythonSemanticAdapter {
    type Parsed<'record> = ParsedPythonRecord<'record>;
    type Admitted<'record> = AdmittedPythonRecord<'record>;
    type Error = PythonSemanticError;

    fn parse<'record>(
        &'record self,
        record: &'record NativeRecord,
    ) -> Result<Self::Parsed<'record>, Self::Error> {
        let payload = match record.kind() {
            NativeRecordKind::Declaration => PythonPayload::Declaration(
                serde_json::from_slice(record.value())
                    .map_err(|_| PythonSemanticError::MalformedJson)?,
            ),
            NativeRecordKind::Type => PythonPayload::Type(
                serde_json::from_slice(record.value())
                    .map_err(|_| PythonSemanticError::MalformedJson)?,
            ),
            NativeRecordKind::Edge => PythonPayload::Edge(
                serde_json::from_slice(record.value())
                    .map_err(|_| PythonSemanticError::MalformedJson)?,
            ),
            NativeRecordKind::Diagnostic => PythonPayload::Diagnostic(
                serde_json::from_slice(record.value())
                    .map_err(|_| PythonSemanticError::MalformedJson)?,
            ),
            NativeRecordKind::Dependency | NativeRecordKind::NegativeDependency => {
                PythonPayload::Import(
                    serde_json::from_slice(record.value())
                        .map_err(|_| PythonSemanticError::MalformedJson)?,
                )
            }
        };
        Ok(ParsedPythonRecord { record, payload })
    }

    fn admit<'record>(
        &'record self,
        parsed: Self::Parsed<'record>,
    ) -> Result<Self::Admitted<'record>, Self::Error> {
        validate_python_payload(&parsed.payload)?;
        Ok(AdmittedPythonRecord(parsed.record))
    }

    fn lower<'record>(
        &'record self,
        admitted: Self::Admitted<'record>,
    ) -> FactRecord<FactKeySchema, FactValueSchema> {
        let record = admitted.0;
        FactRecord::new(
            fact_kind(record.kind()),
            record.key().as_bytes().to_vec(),
            record.value().to_vec(),
        )
    }
}

fn validate_python_payload(payload: &PythonPayload<'_>) -> Result<(), PythonSemanticError> {
    match payload {
        PythonPayload::Declaration(value) => {
            validate_identity(value.owner)?;
            validate_identity(value.name)?;
            validate_span(&value.span)?;
            if value.signature.is_empty() {
                return Err(PythonSemanticError::EmptyIdentity);
            }
            let _ = (
                &value.kind,
                &value.documentation,
                &value.class_form,
                &value.annotations,
            );
            for parameter in &value.parameters {
                validate_identity(parameter.name)?;
                validate_span(&parameter.span)?;
                let _ = (&parameter.kind, parameter.annotation, parameter.has_default);
            }
        }
        PythonPayload::Type(value) => {
            validate_identity(value.owner)?;
            validate_span(&value.span)?;
            if value.inferred.is_empty() || matches!(value.inferred, "Any" | "Unknown") {
                return Err(PythonSemanticError::UnusableType);
            }
            let _ = &value.site;
        }
        PythonPayload::Edge(value) => {
            validate_identity(value.owner)?;
            validate_identity(value.target)?;
            validate_span(&value.span)?;
            if matches!(value.resolution, Resolution::Foreign)
                && value.module.is_none_or(str::is_empty)
            {
                return Err(PythonSemanticError::InvalidResolution);
            }
        }
        PythonPayload::Diagnostic(value) => {
            validate_identity(value.code)?;
            validate_identity(value.message)?;
            validate_span(&value.span)?;
            let _ = &value.severity;
        }
        PythonPayload::Import(value) => {
            validate_identity(value.binding)?;
            validate_identity(value.module)?;
            validate_span(&value.span)?;
        }
    }
    Ok(())
}

fn validate_identity(value: &str) -> Result<(), PythonSemanticError> {
    if value.trim().is_empty() {
        return Err(PythonSemanticError::EmptyIdentity);
    }
    Ok(())
}

const fn validate_span(span: &SourceSpan) -> Result<(), PythonSemanticError> {
    if span.start >= span.end {
        return Err(PythonSemanticError::InvalidSpan);
    }
    Ok(())
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

/// Builds the zero-toolchain local Python syntax frontend.
///
/// This structural baseline never claims native semantic authority.
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
    profile: PythonVersion,
    config: Vec<u8>,
    template: Option<NativeTemplate>,
}

impl PythonFrontend {
    /// Creates a Python authority backed by the source-distributed CPython
    /// AST and symbol helper.
    ///
    /// # Errors
    /// Returns an error when the interpreter, profile, or process contract is invalid.
    pub fn bundled(
        source: Vec<u8>,
        python: impl Into<String>,
        profile: impl Into<String>,
        config: Vec<u8>,
    ) -> Result<Self, AuthorityError> {
        let python = python.into();
        let profile = parse_profile(&profile.into())?;
        validate_absolute(&python, "Python executable")?;
        let helper = Path::new(env!("CARGO_MANIFEST_DIR")).join("helper/semantic.py");
        let template = NativeTemplate::script_source(
            LANGUAGE,
            &helper,
            &python,
            backend_compile::native_executable_id(Path::new(&python)),
            ProtocolDescriptor::cold(),
            default_native_limits()?,
        )?;
        Ok(Self {
            source,
            python,
            pyrefly: helper.to_string_lossy().into_owned(),
            profile,
            config,
            template: Some(template),
        })
    }

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
        let profile = profile.into();
        let profile = parse_profile(&profile)?;
        validate_absolute(&python, "Python executable")?;
        validate_absolute(&pyrefly, "pyrefly authority helper")?;
        Ok(Self {
            source,
            python,
            pyrefly,
            profile,
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
        let profile = parse_profile(&profile)?;
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
                self.profile.name().as_bytes(),
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
            self.profile,
            &self.config,
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
            &PythonSemanticAdapter,
        )
    }
}

fn request_inputs(
    source: &[u8],
    python: &str,
    pyrefly: &str,
    profile: PythonVersion,
    config: &[u8],
) -> Result<Vec<NativeRequestInput>, AuthorityError> {
    Ok(vec![
        native_input("module.py", source.to_vec())?,
        native_input("python-profile", profile.name().as_bytes().to_vec())?,
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

fn parse_profile(profile: &str) -> Result<PythonVersion, AuthorityError> {
    PythonVersion::try_from(profile).map_err(discovery)
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
mod tests {
    use super::*;
    use backend_compile::{Authority, Coverage, FlowSchema, ProfileSchema, SemanticBasisSchema};
    use std::{error::Error, path::PathBuf};

    const PYTHON: &str = "/usr/bin/python3";
    const SOURCE: &[u8] = b"from package import Item as Imported\n\nclass Cafe:\n    def title(self, value: str) -> str:\n        return Imported(value)\n";

    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join("semantic_authority.py")
    }

    fn session_key(frontend: &PythonFrontend, manifest: &InputManifest) -> SessionKey {
        SessionKey::new(
            frontend.identity(),
            manifest,
            typed_of::<ProfileSchema>(b"python-3.14"),
            typed_of::<FlowSchema>(b"python-fixture-flow"),
            typed_of::<SemanticBasisSchema>(b"python-fixture-semantic"),
        )
    }

    #[test]
    fn semantic_admission_rejects_any_and_inverted_spans() -> Result<(), Box<dyn Error>> {
        let any = NativeRecord::new(
            NativeRecordKind::Type,
            "value",
            br#"{"owner":"f","site":"parameter","span":{"start":1,"end":2},"inferred":"Any"}"#
                .to_vec(),
        )?;
        let parsed = PythonSemanticAdapter.parse(&any)?;
        assert!(matches!(
            PythonSemanticAdapter.admit(parsed),
            Err(PythonSemanticError::UnusableType)
        ));

        let edge = NativeRecord::new(
            NativeRecordKind::Edge,
            "f->g",
            br#"{"owner":"f","target":"g","span":{"start":8,"end":3},"resolution":"local","module":null}"#.to_vec(),
        )?;
        let parsed = PythonSemanticAdapter.parse(&edge)?;
        assert!(matches!(
            PythonSemanticAdapter.admit(parsed),
            Err(PythonSemanticError::InvalidSpan)
        ));
        Ok(())
    }

    #[test]
    fn pyrefly_fixture_preserves_all_python_semantic_families() -> Result<(), Box<dyn Error>> {
        let frontend = PythonFrontend::with_helper(
            SOURCE.to_vec(),
            PYTHON,
            fixture().to_string_lossy().into_owned(),
            "3.14",
            b"project-includes=package".to_vec(),
        )?;
        let snapshot = frontend.discover()?;
        let extraction =
            frontend.extract(&snapshot, session_key(&frontend, snapshot.manifest()))?;
        assert!(matches!(extraction.coverage().state(), Coverage::Complete));
        let records = extraction.records();
        assert_eq!(records.len(), 7);
        assert_eq!(records[0].kind(), FactKind::Declaration);
        assert_eq!(records[1].kind(), FactKind::Declaration);
        assert_eq!(records[2].kind(), FactKind::Type);
        assert_eq!(records[3].kind(), FactKind::Edge);
        assert_eq!(records[4].kind(), FactKind::Diagnostic);
        assert_eq!(records[5].kind(), FactKind::Dependency);
        assert_eq!(records[6].kind(), FactKind::NegativeDependency);
        assert!(records.iter().all(|record| record.evidence().is_some()));

        let declaration: serde_json::Value = serde_json::from_slice(records[1].value())?;
        assert_eq!(declaration["name"], "title");
        assert_eq!(declaration["parameters"][1]["annotation"], "str");
        assert_eq!(declaration["documentation"], "method docs");
        let edge: serde_json::Value = serde_json::from_slice(records[3].value())?;
        assert_eq!(edge["owner"], "Café.title");
        assert_eq!(edge["resolution"], "foreign");
        assert_eq!(edge["module"], "package");
        Ok(())
    }
}
