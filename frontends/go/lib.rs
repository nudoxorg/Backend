//! Go `go/packages` native authority adapter.
#![forbid(unsafe_code)]

use backend_compile::{
    Authority, AuthorityError, AuthorityIdentity, DiscoverySnapshot, Extraction, FactKeySchema,
    FactKind, FactRecord, FactValueSchema, Input, InputKind, InputManifest, NativeRecord,
    NativeRecordKind, NativeRequestInput, NativeSemanticAdapter, NativeSemanticRequest,
    NativeTemplate, ProcessLimits, ProtocolDescriptor, SessionKey, SupervisedCommand,
    default_native_limits, extract_native_with_adapter, native_helper_evidence, native_input,
    native_semantic_evidence, native_semantic_input, typed_of,
};
use std::{fmt, path::Path};

const LANGUAGE: &str = "go";
const PAYLOAD_VERSION: &str = "go-semantic-v1";

struct GoSemanticAdapter;

#[derive(Clone, Copy)]
struct ParsedGoRecord<'record> {
    kind: NativeRecordKind,
    key: &'record str,
    bytes: &'record [u8],
    payload: GoPayload<'record>,
}

#[derive(Clone, Copy)]
struct AdmittedGoRecord<'record>(ParsedGoRecord<'record>);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GoSemanticError {
    NonUtf8,
    WrongVersion,
    WrongShape,
    EmptyKey,
    EmptyField,
    InvalidNumber,
    InvalidSpan,
    InvalidEdge,
    InvalidResolution,
    InvalidDependencyState,
    InvalidGoKey,
}

#[derive(Clone, Copy)]
enum GoPayload<'record> {
    Declaration {
        kind: &'record str,
        package: &'record str,
        signature: &'record str,
        start: u32,
        end: u32,
    },
    Type {
        owner: &'record str,
        signature: &'record str,
    },
    Reference {
        owner: &'record str,
        target: &'record str,
        start: u32,
        end: u32,
        resolution: &'record str,
    },
    Diagnostic {
        severity: &'record str,
        code: &'record str,
        start: u32,
        end: u32,
        message: &'record str,
    },
    Package {
        path: &'record str,
        state: &'record str,
    },
}

impl fmt::Display for GoSemanticError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NonUtf8 => "Go semantic payload is not UTF-8",
            Self::WrongVersion => "Go semantic payload has the wrong schema version",
            Self::WrongShape => "Go semantic payload has the wrong field shape",
            Self::EmptyKey => "Go semantic key is empty",
            Self::EmptyField => "Go semantic payload has an empty required field",
            Self::InvalidNumber => "Go semantic payload has an invalid numeric field",
            Self::InvalidSpan => "Go semantic payload has an invalid source span",
            Self::InvalidEdge => "Go semantic edge has no source and target",
            Self::InvalidResolution => "Go semantic reference has an invalid resolution",
            Self::InvalidDependencyState => "Go dependency state is invalid",
            Self::InvalidGoKey => "Go semantic key has an invalid package coordinate",
        })
    }
}

impl std::error::Error for GoSemanticError {}

impl NativeSemanticAdapter for GoSemanticAdapter {
    type Parsed<'record> = ParsedGoRecord<'record>;
    type Admitted<'record> = AdmittedGoRecord<'record>;
    type Error = GoSemanticError;

    fn parse<'record>(
        &'record self,
        record: &'record NativeRecord,
    ) -> Result<Self::Parsed<'record>, Self::Error> {
        let value = std::str::from_utf8(record.value()).map_err(|_| GoSemanticError::NonUtf8)?;
        let fields = value.split('\0').collect::<Vec<_>>();
        if fields.first().copied() != Some(PAYLOAD_VERSION) {
            return Err(GoSemanticError::WrongVersion);
        }
        let number = |value: &str| {
            value
                .parse::<u32>()
                .map_err(|_| GoSemanticError::InvalidNumber)
        };
        let payload = match (record.kind(), fields.as_slice()) {
            (NativeRecordKind::Declaration, [_, kind, package, signature, _, start, end]) => {
                GoPayload::Declaration {
                    kind,
                    package,
                    signature,
                    start: number(start)?,
                    end: number(end)?,
                }
            }
            (NativeRecordKind::Type, [_, owner, signature]) => GoPayload::Type { owner, signature },
            (NativeRecordKind::Edge, [_, owner, target, start, end, resolution]) => {
                GoPayload::Reference {
                    owner,
                    target,
                    start: number(start)?,
                    end: number(end)?,
                    resolution,
                }
            }
            (NativeRecordKind::Diagnostic, [_, severity, code, start, end, message]) => {
                GoPayload::Diagnostic {
                    severity,
                    code,
                    start: number(start)?,
                    end: number(end)?,
                    message,
                }
            }
            (
                NativeRecordKind::Dependency | NativeRecordKind::NegativeDependency,
                [_, "package", path, state],
            ) => GoPayload::Package { path, state },
            _ => return Err(GoSemanticError::WrongShape),
        };
        Ok(ParsedGoRecord {
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
        if parsed.key.is_empty() {
            return Err(GoSemanticError::EmptyKey);
        }
        match parsed.payload {
            GoPayload::Declaration {
                kind,
                package,
                signature,
                start,
                end,
            } => {
                required(&[kind, package, signature])?;
                valid_span(start, end)?;
                admit_go_coordinate(parsed.key)?;
            }
            GoPayload::Type { owner, signature } => {
                required(&[owner, signature])?;
                admit_go_coordinate(parsed.key)?;
            }
            GoPayload::Reference {
                owner,
                target,
                start,
                end,
                resolution,
            } => {
                required(&[owner, target])?;
                valid_span(start, end)?;
                if !matches!(resolution, "local" | "foreign" | "unresolved") {
                    return Err(GoSemanticError::InvalidResolution);
                }
                if parsed
                    .key
                    .split_once("->")
                    .is_none_or(|(source, target)| source.is_empty() || target.is_empty())
                {
                    return Err(GoSemanticError::InvalidEdge);
                }
            }
            GoPayload::Diagnostic {
                severity,
                code,
                start,
                end,
                message,
            } => {
                required(&[code, message])?;
                valid_span(start, end)?;
                if !matches!(severity, "error" | "warning" | "information") {
                    return Err(GoSemanticError::WrongShape);
                }
            }
            GoPayload::Package { path, state } => {
                required(&[path])?;
                admit_go_coordinate(parsed.key)?;
                let expected = match parsed.kind {
                    NativeRecordKind::Dependency => "present",
                    NativeRecordKind::NegativeDependency => "absent",
                    _ => return Err(GoSemanticError::WrongShape),
                };
                if state != expected {
                    return Err(GoSemanticError::InvalidDependencyState);
                }
            }
        }
        Ok(AdmittedGoRecord(parsed))
    }

    fn lower<'record>(
        &'record self,
        admitted: Self::Admitted<'record>,
    ) -> FactRecord<FactKeySchema, FactValueSchema> {
        let record = admitted.0;
        FactRecord::new(
            fact_kind(record.kind),
            record.key.as_bytes().to_vec(),
            record.bytes.to_vec(),
        )
    }
}

fn admit_go_coordinate(key: &str) -> Result<(), GoSemanticError> {
    if key.starts_with("go/") && !valid_go_coordinate(key) {
        Err(GoSemanticError::InvalidGoKey)
    } else {
        Ok(())
    }
}

fn required(fields: &[&str]) -> Result<(), GoSemanticError> {
    if fields.iter().any(|field| field.is_empty()) {
        Err(GoSemanticError::EmptyField)
    } else {
        Ok(())
    }
}

fn valid_span(start: u32, end: u32) -> Result<(), GoSemanticError> {
    if start <= end {
        Ok(())
    } else {
        Err(GoSemanticError::InvalidSpan)
    }
}

fn valid_go_coordinate(key: &str) -> bool {
    key.split('/').all(|segment| {
        !segment.is_empty()
            && segment != "."
            && segment != ".."
            && !segment.contains('\\')
            && !segment.bytes().any(|byte| byte.is_ascii_control())
    })
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

/// Builds the zero-toolchain local Go syntax frontend.
///
/// This structural baseline never claims native semantic authority.
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
    /// Creates a Go authority backed by the source-distributed `go/packages`
    /// helper and the selected verified Go executable.
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
        let helper = Path::new(env!("CARGO_MANIFEST_DIR")).join("helper");
        let template = NativeTemplate::go_source(
            LANGUAGE,
            &helper,
            &go,
            backend_compile::native_executable_id(Path::new(&go)),
            ProtocolDescriptor::cold(),
            default_native_limits()?,
        )?;
        Ok(Self {
            source,
            go,
            modfile,
            tags: tags.into(),
            helper: Some(helper.to_string_lossy().into_owned()),
            template: Some(template),
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
                &native_helper_evidence(Path::new(path)),
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
            producer: typed_of(b"backend-frontend-go-v4"),
            toolchain: backend_compile::native_executable_id(Path::new(&self.go)),
            contract: typed_of(b"native-semantic-adapter-v2/go-semantic-v1"),
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
            &GoSemanticAdapter,
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
            native_helper_evidence(Path::new(helper)),
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
