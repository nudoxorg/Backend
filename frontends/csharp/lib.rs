//! C# Roslyn native authority adapter.
#![forbid(unsafe_code)]

use backend_compile::{
    Authority, AuthorityError, AuthorityIdentity, DiscoverySnapshot, Extraction, FactKeySchema,
    FactKind, FactRecord, FactValueSchema, Input, InputKind, InputManifest, NativeRecord,
    NativeRecordKind, NativeRequestInput, NativeSemanticAdapter, NativeSemanticRequest,
    NativeTemplate, ProcessLimits, ProtocolDescriptor, SessionKey, SupervisedCommand,
    default_native_limits, extract_native_with_adapter, native_input, native_semantic_evidence,
    native_semantic_input, typed_of,
};
use std::{fmt, path::Path};

#[path = "src/legacy/mod.rs"]
pub mod legacy;

const LANGUAGE: &str = "csharp";
const PAYLOAD_VERSION: &str = "csharp-semantic-v1";

struct CSharpSemanticAdapter;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CSharpPayload<'a> {
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
struct ParsedCSharpRecord<'a> {
    kind: NativeRecordKind,
    key: &'a str,
    bytes: &'a [u8],
    payload: CSharpPayload<'a>,
}

#[derive(Clone, Copy)]
struct AdmittedCSharpRecord<'a>(ParsedCSharpRecord<'a>);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CSharpSemanticError {
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

impl fmt::Display for CSharpSemanticError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NonUtf8 => "C# semantic payload is not UTF-8",
            Self::WrongVersion => "C# semantic payload has the wrong schema version",
            Self::WrongShape => "C# semantic payload has the wrong field shape",
            Self::EmptyField => "C# semantic payload has an empty required field",
            Self::InvalidNumber => "C# semantic payload has an invalid numeric field",
            Self::InvalidSpan => "C# semantic payload has an invalid source span",
            Self::InvalidSeverity => "C# semantic diagnostic has an invalid severity",
            Self::InvalidDependencyState => "C# package observation has an invalid state",
            Self::InvalidTypeSignature => "C# semantic type signature is invalid",
        })
    }
}

impl std::error::Error for CSharpSemanticError {}

impl NativeSemanticAdapter for CSharpSemanticAdapter {
    type Parsed<'record> = ParsedCSharpRecord<'record>;
    type Admitted<'record> = AdmittedCSharpRecord<'record>;
    type Error = CSharpSemanticError;

    fn parse<'record>(
        &'record self,
        record: &'record NativeRecord,
    ) -> Result<Self::Parsed<'record>, Self::Error> {
        let value =
            std::str::from_utf8(record.value()).map_err(|_| CSharpSemanticError::NonUtf8)?;
        let fields = value.split('\0').collect::<Vec<_>>();
        if fields.first().copied() != Some(PAYLOAD_VERSION) {
            return Err(CSharpSemanticError::WrongVersion);
        }
        let payload = match (record.kind(), fields.as_slice()) {
            (NativeRecordKind::Declaration, [_, kind, owner, signature, documentation]) => {
                CSharpPayload::Declaration {
                    kind,
                    owner,
                    signature,
                    documentation,
                }
            }
            (NativeRecordKind::Type, [_, owner, signature]) => {
                CSharpPayload::Type { owner, signature }
            }
            (NativeRecordKind::Edge, [_, owner, target, start, end]) => CSharpPayload::Reference {
                owner,
                target,
                start: start
                    .parse()
                    .map_err(|_| CSharpSemanticError::InvalidNumber)?,
                end: end
                    .parse()
                    .map_err(|_| CSharpSemanticError::InvalidNumber)?,
            },
            (NativeRecordKind::Diagnostic, [_, severity, code, start, end, message]) => {
                CSharpPayload::Diagnostic {
                    severity,
                    code,
                    start: start
                        .parse()
                        .map_err(|_| CSharpSemanticError::InvalidNumber)?,
                    end: end
                        .parse()
                        .map_err(|_| CSharpSemanticError::InvalidNumber)?,
                    message,
                }
            }
            (
                NativeRecordKind::Dependency | NativeRecordKind::NegativeDependency,
                [_, "package", name, state],
            ) => CSharpPayload::Package { name, state },
            _ => return Err(CSharpSemanticError::WrongShape),
        };
        Ok(ParsedCSharpRecord {
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
            CSharpPayload::Declaration {
                kind,
                owner,
                signature,
                documentation: _,
            } => {
                required(&[kind, owner, signature])?;
                validate_type_signature(signature)?;
            }
            CSharpPayload::Type { owner, signature } => {
                required(&[owner, signature])?;
                validate_type_signature(signature)?;
            }
            CSharpPayload::Reference {
                owner,
                target,
                start,
                end,
            } => {
                required(&[owner, target])?;
                valid_span(start, end)?;
            }
            CSharpPayload::Diagnostic {
                severity,
                code,
                start,
                end,
                message,
            } => {
                required(&[code, message])?;
                if !matches!(severity, "hidden" | "info" | "warning" | "error") {
                    return Err(CSharpSemanticError::InvalidSeverity);
                }
                valid_span(start, end)?;
            }
            CSharpPayload::Package { name, state } => {
                required(&[name])?;
                let expected = match parsed.kind {
                    NativeRecordKind::Dependency => "present",
                    NativeRecordKind::NegativeDependency => "absent",
                    _ => return Err(CSharpSemanticError::WrongShape),
                };
                if state != expected {
                    return Err(CSharpSemanticError::InvalidDependencyState);
                }
            }
        }
        Ok(AdmittedCSharpRecord(parsed))
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

fn required(fields: &[&str]) -> Result<(), CSharpSemanticError> {
    if fields.iter().any(|field| field.is_empty()) {
        Err(CSharpSemanticError::EmptyField)
    } else {
        Ok(())
    }
}

fn valid_span(start: u32, end: u32) -> Result<(), CSharpSemanticError> {
    if start <= end {
        Ok(())
    } else {
        Err(CSharpSemanticError::InvalidSpan)
    }
}

fn validate_type_signature(signature: &str) -> Result<(), CSharpSemanticError> {
    let mut stack = Vec::new();
    for byte in signature.bytes() {
        match byte {
            b'<' | b'[' | b'(' => {
                if stack.len() == 64 {
                    return Err(CSharpSemanticError::InvalidTypeSignature);
                }
                stack.push(byte);
            }
            b'>' if stack.pop() != Some(b'<') => {
                return Err(CSharpSemanticError::InvalidTypeSignature);
            }
            b']' if stack.pop() != Some(b'[') => {
                return Err(CSharpSemanticError::InvalidTypeSignature);
            }
            b')' if stack.pop() != Some(b'(') => {
                return Err(CSharpSemanticError::InvalidTypeSignature);
            }
            0..=31 | 127 => return Err(CSharpSemanticError::InvalidTypeSignature),
            _ => {}
        }
    }
    if stack.is_empty() {
        Ok(())
    } else {
        Err(CSharpSemanticError::InvalidTypeSignature)
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

/// Builds the zero-toolchain local C# syntax frontend.
///
/// This structural baseline never claims native semantic authority.
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
            &CSharpSemanticAdapter,
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
    fn admits_roslyn_declarations_recursive_types_references_docs_diagnostics_and_packages()
    -> Result<(), Box<dyn Error>> {
        let cases = [
            record(
                NativeRecordKind::Declaration,
                "Demo.Node<T>",
                &[
                    PAYLOAD_VERSION,
                    "namedtype",
                    "Demo",
                    "global::Demo.Node<global::Demo.Node<T[]>>",
                    "<summary>Recursive node.</summary>",
                ],
            )?,
            record(
                NativeRecordKind::Type,
                "Demo.Node<T>",
                &[
                    PAYLOAD_VERSION,
                    "Demo.Node<T>",
                    "global::Demo.Node<global::Demo.Node<T[]>>",
                ],
            )?,
            record(
                NativeRecordKind::Edge,
                "Demo.Run()->Demo.Make()@8:12",
                &[PAYLOAD_VERSION, "Demo.Run()", "Demo.Make()", "8", "12"],
            )?,
            record(
                NativeRecordKind::Diagnostic,
                "CS0618@8:12",
                &[
                    PAYLOAD_VERSION,
                    "warning",
                    "CS0618",
                    "8",
                    "12",
                    "member is obsolete",
                ],
            )?,
            record(
                NativeRecordKind::Dependency,
                "package:System.Runtime",
                &[
                    PAYLOAD_VERSION,
                    "package",
                    "System.Runtime, Version=8.0.0.0",
                    "present",
                ],
            )?,
            record(
                NativeRecordKind::NegativeDependency,
                "package:optional",
                &[PAYLOAD_VERSION, "package", "optional", "absent"],
            )?,
        ];
        for native in &cases {
            let parsed = CSharpSemanticAdapter.parse(native)?;
            let admitted = CSharpSemanticAdapter.admit(parsed)?;
            let lowered = CSharpSemanticAdapter.lower(admitted);
            assert_eq!(lowered.value(), native.value());
        }
        Ok(())
    }

    #[test]
    fn rejects_unbalanced_recursive_type_and_false_package_claim() -> Result<(), Box<dyn Error>> {
        let malformed = record(
            NativeRecordKind::Type,
            "Node",
            &[PAYLOAD_VERSION, "Node", "Node<Node<T>"],
        )?;
        assert!(matches!(
            CSharpSemanticAdapter.admit(CSharpSemanticAdapter.parse(&malformed)?),
            Err(CSharpSemanticError::InvalidTypeSignature)
        ));
        let package = record(
            NativeRecordKind::Dependency,
            "package:x",
            &[PAYLOAD_VERSION, "package", "x", "absent"],
        )?;
        assert!(matches!(
            CSharpSemanticAdapter.admit(CSharpSemanticAdapter.parse(&package)?),
            Err(CSharpSemanticError::InvalidDependencyState)
        ));
        Ok(())
    }
}
