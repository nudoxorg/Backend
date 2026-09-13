//! Rust Cargo/rust-analyzer native authority adapter.
#![forbid(unsafe_code)]

use backend_compile::{
    Authority, AuthorityError, AuthorityIdentity, DiscoverySnapshot, Extraction, FactKeySchema,
    FactKind, FactRecord, FactValueSchema, Input, InputKind, InputManifest, MAX_NATIVE_RECORDS,
    NativeRecord, NativeRecordKind, NativeRequestInput, NativeSemanticAdapter,
    NativeSemanticRequest, NativeTemplate, ProcessLimits, ProtocolDescriptor, SessionKey,
    SupervisedCommand, default_native_limits, extract_native_with_adapter, native_input,
    native_semantic_evidence, native_semantic_input, typed_of,
};
use ra_ap_hir::PathResolution;
use ra_ap_syntax::AstNode;
use std::{
    fmt,
    path::{Path, PathBuf},
    process::Command,
};

mod authority;
pub mod purl;

pub use authority::{
    ByteSpan, ModuleDeclaration, RustAnalysisControl, RustAuthority, RustAuthorityError,
    RustDeclaration, RustDefinition, RustFeatureControl, RustFieldAccess, RustMethodCall,
    RustProject, SemanticKind, SourceByteLimit, SourceOrigin,
};
pub use purl::{RustLocatedPackage, RustPackageUrl, RustPurlError};

const NATIVE_PAYLOAD_PREFIX: &str = "rust-semantic-v2";

/// Native compiler identity and sysroot accepted for one Rust authority transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RustToolchain {
    /// Caller-selected compiler executable.
    pub tool: PathBuf,
    /// Sysroot reported by exactly that compiler executable.
    pub sysroot: PathBuf,
}

impl RustToolchain {
    /// Admits caller-selected absolute Rust compiler and sysroot paths.
    ///
    /// # Errors
    /// Returns [`LoadError`] when either path is not an absolute usable path.
    pub fn from_paths(tool: PathBuf, sysroot: PathBuf) -> Result<Self, LoadError> {
        if !tool.is_absolute() {
            return Err(LoadError::RelativeTool { tool });
        }
        if !sysroot.is_absolute() {
            return Err(LoadError::RelativeSysroot { sysroot });
        }
        if !tool.is_file() {
            return Err(LoadError::InvalidTool { path: tool });
        }
        if !sysroot.is_dir() {
            return Err(LoadError::InvalidSysroot { path: sysroot });
        }
        Ok(Self { tool, sysroot })
    }

    /// Discovers the sysroot from one explicitly selected Rust compiler.
    ///
    /// # Errors
    /// Returns [`LoadError`] when the executable cannot report a usable sysroot.
    pub fn discover(tool: impl Into<PathBuf>) -> Result<Self, LoadError> {
        let tool = tool.into();
        if !tool.is_absolute() {
            return Err(LoadError::RelativeTool { tool });
        }
        let output = Command::new(&tool)
            .args(["--print", "sysroot"])
            .output()
            .map_err(|source| LoadError::SysrootQuery {
                tool: tool.clone(),
                source,
            })?;
        if !output.status.success() {
            return Err(LoadError::SysrootUnavailable { tool });
        }
        let sysroot = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
        if !sysroot.is_dir() {
            return Err(LoadError::InvalidSysroot { path: sysroot });
        }
        Ok(Self { tool, sysroot })
    }
}

/// Failure to establish the native toolchain required by the Rust authority.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    /// A relative compiler path was supplied.
    #[error("Rust compiler path is not absolute: {tool}")]
    RelativeTool {
        /// Caller-selected compiler path.
        tool: PathBuf,
    },
    /// A relative sysroot path was supplied.
    #[error("Rust sysroot path is not absolute: {sysroot}")]
    RelativeSysroot {
        /// Caller-selected sysroot path.
        sysroot: PathBuf,
    },
    /// The compiler path does not name a regular file.
    #[error("configured Rust compiler is not a file: {path}")]
    InvalidTool {
        /// Compiler path that failed the regular-file check.
        path: PathBuf,
    },
    /// The compiler process could not be started.
    #[error("cannot run {tool} for Rust sysroot discovery: {source}")]
    SysrootQuery {
        /// Compiler executable queried for its sysroot.
        tool: PathBuf,
        /// Original process-start failure.
        #[source]
        source: std::io::Error,
    },
    /// The compiler did not successfully report a sysroot.
    #[error("{tool} did not provide a Rust sysroot")]
    SysrootUnavailable {
        /// Compiler executable that returned failure.
        tool: PathBuf,
    },
    /// The reported sysroot is not a directory.
    #[error("reported Rust sysroot is not a directory: {path}")]
    InvalidSysroot {
        /// Reported sysroot path.
        path: PathBuf,
    },
}

const LANGUAGE: &str = "rust";

struct RustSemanticAdapter;

#[derive(Clone, Copy)]
struct ParsedRustRecord<'record> {
    kind: NativeRecordKind,
    key: &'record str,
    value: &'record str,
}

#[derive(Clone, Copy)]
struct AdmittedRustRecord<'record>(ParsedRustRecord<'record>);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RustSemanticError {
    NonUtf8Value,
    EmptyValue,
    InvalidEdge,
    InvalidDependencyState,
}

impl fmt::Display for RustSemanticError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NonUtf8Value => "Rust semantic value is not UTF-8",
            Self::EmptyValue => "Rust semantic value is empty",
            Self::InvalidEdge => "Rust semantic edge has no source and target",
            Self::InvalidDependencyState => "Rust dependency state is invalid",
        })
    }
}

impl std::error::Error for RustSemanticError {}

impl NativeSemanticAdapter for RustSemanticAdapter {
    type Parsed<'record> = ParsedRustRecord<'record>;
    type Admitted<'record> = AdmittedRustRecord<'record>;
    type Error = RustSemanticError;

    fn parse<'record>(
        &'record self,
        record: &'record NativeRecord,
    ) -> Result<Self::Parsed<'record>, Self::Error> {
        let value =
            std::str::from_utf8(record.value()).map_err(|_| RustSemanticError::NonUtf8Value)?;
        Ok(ParsedRustRecord {
            kind: record.kind(),
            key: record.key(),
            value,
        })
    }

    fn admit<'record>(
        &'record self,
        parsed: Self::Parsed<'record>,
    ) -> Result<Self::Admitted<'record>, Self::Error> {
        if parsed.value.is_empty() {
            return Err(RustSemanticError::EmptyValue);
        }
        match parsed.kind {
            NativeRecordKind::Edge
                if parsed
                    .key
                    .split_once("->")
                    .is_none_or(|(source, target)| source.is_empty() || target.is_empty()) =>
            {
                Err(RustSemanticError::InvalidEdge)
            }
            NativeRecordKind::Dependency if parsed.value != "present" => {
                Err(RustSemanticError::InvalidDependencyState)
            }
            NativeRecordKind::NegativeDependency if parsed.value != "absent" => {
                Err(RustSemanticError::InvalidDependencyState)
            }
            _ => Ok(AdmittedRustRecord(parsed)),
        }
    }

    fn lower<'record>(
        &'record self,
        admitted: Self::Admitted<'record>,
    ) -> FactRecord<FactKeySchema, FactValueSchema> {
        let record = admitted.0;
        FactRecord::new(
            fact_kind(record.kind),
            record.key.as_bytes().to_vec(),
            record.value.as_bytes().to_vec(),
        )
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

/// Runs the in-process rust-analyzer authority and lowers its owned semantic
/// observations into the common native record boundary.
///
/// The callback-based authority keeps HIR borrows inside one transaction. The
/// returned records own only bounded protocol bytes, so callers may safely
/// cache or transmit them after rust-analyzer has been released.
///
/// # Errors
///
/// Returns [`RustAuthorityError`] when Cargo loading, source admission,
/// semantic projection, cancellation, or native record bounds fail.
pub fn native_records(
    project: &RustProject,
    control: RustAnalysisControl<'_>,
    features: RustFeatureControl<'_>,
) -> Result<Vec<NativeRecord>, RustAuthorityError> {
    project.analyze_with_features(control, features, |authority| {
        project_native_records(&authority, &project.source_path)
    })
}

fn project_native_records(
    authority: &RustAuthority<'_>,
    source_path: &Path,
) -> Result<Vec<NativeRecord>, RustAuthorityError> {
    let mut projection = NativeProjection::default();
    projection.declarations(authority)?;
    projection.paths(authority)?;
    projection.methods(authority)?;
    projection.fields(authority)?;
    projection.macros(authority)?;
    projection.diagnostics(authority)?;
    projection.source(source_path)?;
    Ok(projection.finish())
}

#[derive(Default)]
struct NativeProjection {
    records: Vec<NativeRecord>,
    keys: std::collections::BTreeSet<(NativeRecordKind, String)>,
}

impl NativeProjection {
    fn declarations(&mut self, authority: &RustAuthority<'_>) -> Result<(), RustAuthorityError> {
        for declaration in authority.module_declarations() {
            let Some(item_span) = local_span(declaration.item) else {
                continue;
            };
            let name = declaration
                .name
                .map(|span| source_text(authority, span))
                .transpose()?;
            let kind = semantic_kind(declaration.definition.kind());
            let declaration_key = format!(
                "rust/declaration/{kind}/{}/{}",
                item_span.start, item_span.end,
            );
            self.push(
                NativeRecordKind::Declaration,
                declaration_key.clone(),
                payload(&[
                    ("kind", kind),
                    ("origin", "local"),
                    ("name", name.as_deref().unwrap_or("<anonymous>")),
                    ("span-start", &item_span.start.to_string()),
                    ("span-end", &item_span.end.to_string()),
                ]),
            )?;
            if declaration
                .definition
                .semantic_type(authority.database)
                .is_some()
            {
                self.push(
                    NativeRecordKind::Type,
                    format!("{declaration_key}/type"),
                    payload(&[
                        ("kind", "hir"),
                        ("state", "resolved"),
                        ("owner", declaration_key.as_str()),
                    ]),
                )?;
            }
        }
        Ok(())
    }

    fn paths(&mut self, authority: &RustAuthority<'_>) -> Result<(), RustAuthorityError> {
        for (index, path) in authority.top_level_paths().enumerate() {
            let Some(span) = authority.projected_span(path.syntax())? else {
                continue;
            };
            let Some((resolution, substitution)) = authority.resolve_path(&path) else {
                continue;
            };
            let target = path_resolution_kind(resolution);
            self.push(
                NativeRecordKind::Edge,
                format!("rust/path/{}/{}->{target}", span.start, index),
                payload(&[
                    ("relation", "path-resolution"),
                    ("target-kind", target),
                    (
                        "substitution",
                        if substitution.is_some() {
                            "present"
                        } else {
                            "absent"
                        },
                    ),
                ]),
            )?;
        }
        Ok(())
    }

    fn methods(&mut self, authority: &RustAuthority<'_>) -> Result<(), RustAuthorityError> {
        for (index, call) in authority.method_calls().enumerate() {
            let Some(span) = call
                .projected_span
                .or(authority.projected_span(call.syntax.syntax())?)
            else {
                continue;
            };
            if call.target.is_none() {
                continue;
            }
            self.push(
                NativeRecordKind::Edge,
                format!("rust/method/{}/{}->function", span.start, index),
                payload(&[("relation", "method-call"), ("target-kind", "function")]),
            )?;
        }
        Ok(())
    }

    fn fields(&mut self, authority: &RustAuthority<'_>) -> Result<(), RustAuthorityError> {
        for (index, access) in authority.field_accesses().enumerate() {
            let Some(target) = access.target else {
                continue;
            };
            let Some(span) = authority.projected_span(access.syntax.syntax())? else {
                continue;
            };
            let origin = authority.definition_origin(target, SemanticKind::Field);
            let target = if origin.is_local() {
                "field-local"
            } else {
                "field-foreign"
            };
            self.push(
                NativeRecordKind::Edge,
                format!("rust/field/{}/{}->{target}", span.start, index),
                payload(&[("relation", "field-access"), ("target-kind", target)]),
            )?;
        }
        Ok(())
    }

    fn macros(&mut self, authority: &RustAuthority<'_>) -> Result<(), RustAuthorityError> {
        for (index, call) in authority.macro_calls().enumerate() {
            let Some(span) = authority.projected_span(call.syntax())? else {
                continue;
            };
            if authority.resolve_macro(&call).is_none() {
                continue;
            }
            self.push(
                NativeRecordKind::Declaration,
                format!("rust/macro/{}/{}", span.start, index),
                payload(&[
                    ("kind", "macro"),
                    ("origin", "local"),
                    ("span-start", &span.start.to_string()),
                    ("span-end", &span.end.to_string()),
                ]),
            )?;
        }
        Ok(())
    }

    fn diagnostics(&mut self, authority: &RustAuthority<'_>) -> Result<(), RustAuthorityError> {
        for (index, span) in syntax_errors(authority).into_iter().enumerate() {
            self.push(
                NativeRecordKind::Diagnostic,
                format!("rust/diagnostic/{index}/{}-{}", span.start, span.end),
                payload(&[
                    ("severity", "error"),
                    ("span-start", &span.start.to_string()),
                    ("span-end", &span.end.to_string()),
                ]),
            )?;
        }
        Ok(())
    }

    fn source(&mut self, source_path: &Path) -> Result<(), RustAuthorityError> {
        self.push(
            NativeRecordKind::Dependency,
            format!(
                "rust/source/{}",
                escape_cell(&source_path.to_string_lossy())
            ),
            b"present".to_vec(),
        )
    }

    fn push(
        &mut self,
        kind: NativeRecordKind,
        key: String,
        value: Vec<u8>,
    ) -> Result<(), RustAuthorityError> {
        push_native_record(&mut self.records, &mut self.keys, kind, key, value)
    }

    fn finish(mut self) -> Vec<NativeRecord> {
        self.records
            .sort_by(|left, right| (left.kind(), left.key()).cmp(&(right.kind(), right.key())));
        self.records
    }
}

fn local_span(origin: SourceOrigin) -> Option<ByteSpan> {
    match origin {
        SourceOrigin::Local(span) => Some(span),
        SourceOrigin::Foreign(_) => None,
    }
}

fn source_text(
    authority: &RustAuthority<'_>,
    span: ByteSpan,
) -> Result<String, RustAuthorityError> {
    let bytes = authority.source_at(span)?;
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| RustAuthorityError::Admission {
            cause: format!("Rust declaration span {span:?} is not UTF-8"),
        })
}

fn syntax_errors(authority: &RustAuthority<'_>) -> Vec<ByteSpan> {
    let mut spans = Vec::new();
    authority.visit_syntax_diagnostics(|span| spans.push(span));
    spans
}

fn semantic_kind(kind: SemanticKind) -> &'static str {
    match kind {
        SemanticKind::Module => "module",
        SemanticKind::Function => "function",
        SemanticKind::Field => "field",
        SemanticKind::Record => "record",
        SemanticKind::Enum => "enum",
        SemanticKind::Trait => "trait",
        SemanticKind::Implementation => "implementation",
        SemanticKind::TypeAlias => "type-alias",
        SemanticKind::Constant => "constant",
        SemanticKind::Static => "static",
        SemanticKind::Macro => "macro",
        SemanticKind::Variant => "variant",
        SemanticKind::LocalBinding => "local-binding",
        SemanticKind::GenericParameter => "generic-parameter",
        SemanticKind::Builtin => "builtin",
    }
}

fn path_resolution_kind(resolution: PathResolution) -> &'static str {
    match resolution {
        PathResolution::Def(definition) => match definition {
            ra_ap_hir::ModuleDef::Module(_) => "module",
            ra_ap_hir::ModuleDef::Function(_) => "function",
            ra_ap_hir::ModuleDef::Adt(_) => "record",
            ra_ap_hir::ModuleDef::EnumVariant(_) => "variant",
            ra_ap_hir::ModuleDef::Const(_) => "constant",
            ra_ap_hir::ModuleDef::Static(_) => "static",
            ra_ap_hir::ModuleDef::Trait(_) => "trait",
            ra_ap_hir::ModuleDef::TypeAlias(_) => "type-alias",
            ra_ap_hir::ModuleDef::BuiltinType(_) => "builtin",
            ra_ap_hir::ModuleDef::Macro(_) => "macro",
        },
        PathResolution::Local(_) => "local",
        PathResolution::TypeParam(_) => "type-parameter",
        PathResolution::ConstParam(_) => "const-parameter",
        PathResolution::SelfType(_) => "self-type",
        PathResolution::BuiltinAttr(_) => "builtin-attribute",
        PathResolution::ToolModule(_) => "tool-module",
        PathResolution::DeriveHelper(_) => "derive-helper",
    }
}

fn push_native_record(
    records: &mut Vec<NativeRecord>,
    keys: &mut std::collections::BTreeSet<(NativeRecordKind, String)>,
    kind: NativeRecordKind,
    key: String,
    value: Vec<u8>,
) -> Result<(), RustAuthorityError> {
    if records.len() >= MAX_NATIVE_RECORDS {
        return Err(RustAuthorityError::Admission {
            cause: format!("Rust semantic record set exceeds {MAX_NATIVE_RECORDS} records"),
        });
    }
    if !keys.insert((kind, key.clone())) {
        return Ok(());
    }
    let record =
        NativeRecord::new(kind, key, value).map_err(|error| RustAuthorityError::Admission {
            cause: error.to_string(),
        })?;
    records.push(record);
    Ok(())
}

fn payload(fields: &[(&str, &str)]) -> Vec<u8> {
    let mut output = String::from(NATIVE_PAYLOAD_PREFIX);
    for (name, value) in fields {
        output.push(';');
        output.push_str(name);
        output.push('=');
        output.push_str(&escape_cell(value));
    }
    output.into_bytes()
}

fn escape_cell(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'%' => output.push_str("%25"),
            b';' => output.push_str("%3B"),
            b'=' => output.push_str("%3D"),
            _ => output.push(byte as char),
        }
    }
    output
}

/// Builds the zero-toolchain local Rust syntax frontend.
///
/// This structural baseline never claims native semantic authority.
///
/// # Errors
/// Returns an error when the embedded grammar query cannot be admitted.
pub fn syntax_frontend() -> Result<backend_compile::SyntaxFrontend, backend_compile::SyntaxError> {
    backend_compile::SyntaxFrontend::new(
        backend_compile::SourceLanguage::Rust,
        b"tree-sitter-rust-0.24.2/tags-v1",
        vec![backend_compile::GrammarVariant::new(
            &["rs"],
            tree_sitter_rust::LANGUAGE.into(),
            tree_sitter_rust::TAGS_QUERY,
        )?],
    )
}

/// Rust authority configuration with explicit Cargo feature inputs.
pub struct RustFrontend {
    source: Vec<u8>,
    rustc: String,
    manifest: Vec<u8>,
    features: String,
    helper: Option<String>,
    persistent: bool,
    template: Option<NativeTemplate>,
}

impl RustFrontend {
    /// Creates a manifest-only Rust configuration.
    ///
    /// `rustc` does not emit the backend authority payload. Call
    /// [`Self::with_helper`] or [`Self::with_persistent_helper`] to select an
    /// explicit rust-analyzer/Cargo adapter.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn new(
        source: Vec<u8>,
        rustc: impl Into<String>,
        manifest: Vec<u8>,
        features: impl Into<String>,
    ) -> Result<Self, AuthorityError> {
        let rustc = rustc.into();
        validate_absolute(&rustc, "rustc executable")?;
        Ok(Self {
            source,
            rustc,
            manifest,
            features: features.into(),
            helper: None,
            persistent: false,
            template: None,
        })
    }

    /// Creates a cold Rust authority using an explicit absolute helper.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn with_helper(
        source: Vec<u8>,
        rustc: impl Into<String>,
        helper: impl Into<String>,
        manifest: Vec<u8>,
        features: impl Into<String>,
    ) -> Result<Self, AuthorityError> {
        Self::with_mode_and_limits(
            source,
            rustc,
            helper,
            manifest,
            features,
            false,
            default_native_limits()?,
        )
    }

    /// Creates a persistent Rust authority using an explicit helper.
    ///
    /// The helper must implement the checked `backend-compile` session frame
    /// protocol. Its bounded semantic envelope is emitted on the controlled
    /// payload channel described by [`backend_compile::NativeEnvelope`].
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn with_persistent_helper(
        source: Vec<u8>,
        rustc: impl Into<String>,
        helper: impl Into<String>,
        manifest: Vec<u8>,
        features: impl Into<String>,
    ) -> Result<Self, AuthorityError> {
        Self::with_mode_and_limits(
            source,
            rustc,
            helper,
            manifest,
            features,
            true,
            default_native_limits()?,
        )
    }

    /// Creates a cold Rust authority with an explicit process budget.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn with_helper_and_limits(
        source: Vec<u8>,
        rustc: impl Into<String>,
        helper: impl Into<String>,
        manifest: Vec<u8>,
        features: impl Into<String>,
        limits: ProcessLimits,
    ) -> Result<Self, AuthorityError> {
        Self::with_mode_and_limits(source, rustc, helper, manifest, features, false, limits)
    }

    /// Creates a persistent Rust authority with an explicit process budget.
    /// # Errors
    ///
    /// Returns an error when the compiler input or process configuration is invalid.
    pub fn with_persistent_helper_and_limits(
        source: Vec<u8>,
        rustc: impl Into<String>,
        helper: impl Into<String>,
        manifest: Vec<u8>,
        features: impl Into<String>,
        limits: ProcessLimits,
    ) -> Result<Self, AuthorityError> {
        Self::with_mode_and_limits(source, rustc, helper, manifest, features, true, limits)
    }

    fn with_mode_and_limits(
        source: Vec<u8>,
        rustc: impl Into<String>,
        helper: impl Into<String>,
        manifest: Vec<u8>,
        features: impl Into<String>,
        persistent: bool,
        limits: ProcessLimits,
    ) -> Result<Self, AuthorityError> {
        let rustc = rustc.into();
        let helper = helper.into();
        let features = features.into();
        validate_absolute(&rustc, "rustc executable")?;
        validate_absolute(&helper, "Rust authority helper")?;
        let template = NativeTemplate::new(
            LANGUAGE,
            &helper,
            &rustc,
            backend_compile::native_executable_id(Path::new(&rustc)),
            if persistent {
                ProtocolDescriptor::persistent()
            } else {
                ProtocolDescriptor::cold()
            },
            limits,
        )?;
        Ok(Self {
            source,
            rustc,
            manifest,
            features,
            helper: Some(helper),
            persistent,
            template: Some(template),
        })
    }

    /// Returns the authority capability and reset identity.
    #[must_use]
    pub fn session_capability(&self) -> SessionCapability {
        SessionCapability {
            reset_key: self.identity().digest(),
            persistent: self.persistent,
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
            Input::new(InputKind::Source, "src/lib.rs", &self.source).map_err(discovery)?,
            Input::new(InputKind::Configuration, "Cargo.toml", &self.manifest)
                .map_err(discovery)?,
            Input::new(
                InputKind::Configuration,
                "features",
                self.features.as_bytes(),
            )
            .map_err(discovery)?,
            Input::new(
                InputKind::Toolchain,
                "rustc",
                &backend_compile::native_executable_evidence(Path::new(&self.rustc)),
            )
            .map_err(discovery)?,
            Input::absent(InputKind::NegativeDependency, "target/optional").map_err(discovery)?,
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

impl Authority for RustFrontend {
    fn identity(&self) -> AuthorityIdentity {
        AuthorityIdentity {
            producer: typed_of(b"backend-frontend-rust-v4"),
            toolchain: backend_compile::native_executable_id(Path::new(&self.rustc)),
            contract: if self.persistent {
                typed_of(b"native-semantic-adapter-v2/rust/persistent")
            } else {
                typed_of(b"native-semantic-adapter-v2/rust/cold")
            },
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
            &self.rustc,
            self.helper.as_deref().unwrap_or("unsupported"),
            &self.manifest,
            &self.features,
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
            &RustSemanticAdapter,
        )
    }
}

fn request_inputs(
    source: &[u8],
    rustc: &str,
    helper: &str,
    manifest: &[u8],
    features: &str,
) -> Result<Vec<NativeRequestInput>, AuthorityError> {
    Ok(vec![
        native_input("src/lib.rs", source.to_vec())?,
        native_input("Cargo.toml", manifest.to_vec())?,
        native_input("features", features.as_bytes().to_vec())?,
        native_input(
            "rustc",
            backend_compile::native_executable_evidence(Path::new(rustc)),
        )?,
        native_input(
            "authority-helper",
            backend_compile::native_executable_evidence(Path::new(helper)),
        )?,
        native_input("target/optional", b"absent".to_vec())?,
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
    /// Whether the configured helper advertises persistent sessions.
    pub persistent: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_compile::{
        Authority, AuthorityError, Coverage, ExecutableIdentity, Extraction, FactKind, FlowSchema,
        InputManifest, ProcessError, ProfileSchema, SemanticBasisSchema,
    };
    use std::{
        error::Error,
        path::{Path, PathBuf},
        time::Duration,
    };
    #[cfg(unix)]
    use std::{fs, os::unix::fs::PermissionsExt};

    const TOOLCHAIN: &str = "/bin/sh";
    const SOURCE: &[u8] = b"fn main() { let answer: i32 = 42; }";
    const MANIFEST: &[u8] = b"[package]\nname = \"fixture\"\n";

    #[test]
    fn semantic_adapter_rejects_unproved_dependency_state() -> Result<(), Box<dyn Error>> {
        let record = NativeRecord::new(NativeRecordKind::Dependency, "serde", b"maybe".to_vec())?;
        let parsed = RustSemanticAdapter.parse(&record)?;
        assert!(matches!(
            RustSemanticAdapter.admit(parsed),
            Err(RustSemanticError::InvalidDependencyState)
        ));
        Ok(())
    }

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(name)
    }

    fn session_key(frontend: &RustFrontend, manifest: &InputManifest) -> SessionKey {
        SessionKey::new(
            frontend.identity(),
            manifest,
            typed_of::<ProfileSchema>(b"fixture-profile"),
            typed_of::<FlowSchema>(b"fixture-flow"),
            typed_of::<SemanticBasisSchema>(b"fixture-semantic"),
        )
    }

    fn assert_native_records(extraction: &Extraction) {
        assert!(matches!(extraction.coverage().state(), Coverage::Complete));
        let records = extraction.records();
        assert_eq!(records.len(), 6);
        assert_eq!(records[0].kind(), FactKind::Declaration);
        assert_eq!(records[0].key_bytes(), b"main");
        assert_eq!(records[0].value(), b"decl");
        assert_eq!(records[1].kind(), FactKind::Type);
        assert_eq!(records[1].value(), b"i32");
        assert_eq!(records[2].kind(), FactKind::Edge);
        assert_eq!(records[3].kind(), FactKind::Diagnostic);
        assert_eq!(records[4].kind(), FactKind::Dependency);
        assert_eq!(records[5].kind(), FactKind::NegativeDependency);
        assert!(records.iter().all(|record| record.evidence().is_some()));
        assert!(extraction.facts().is_empty());
    }

    #[test]
    fn cold_fixture_materializes_all_native_record_kinds() -> Result<(), Box<dyn Error>> {
        let frontend = RustFrontend::with_helper(
            SOURCE.to_vec(),
            TOOLCHAIN,
            fixture("native_authority.py")
                .to_string_lossy()
                .into_owned(),
            MANIFEST.to_vec(),
            "default",
        )?;
        let snapshot = frontend.discover()?;
        let key = session_key(&frontend, snapshot.manifest());
        let extraction = frontend.extract(&snapshot, key)?;
        assert_native_records(&extraction);
        assert_eq!(
            frontend.identity().toolchain,
            ExecutableIdentity::from_path(Path::new(TOOLCHAIN))?.digest()
        );
        assert_eq!(
            extraction.manifest_root(),
            Some(snapshot.manifest().digest())
        );
        assert_eq!(
            extraction.authority_root(),
            Some(frontend.identity().digest())
        );
        assert_eq!(extraction.revision(), Some(snapshot.sequence()));
        Ok(())
    }

    #[test]
    fn persistent_fixture_uses_framed_request_and_bound_payload() -> Result<(), Box<dyn Error>> {
        let frontend = RustFrontend::with_persistent_helper(
            SOURCE.to_vec(),
            TOOLCHAIN,
            fixture("native_authority.py")
                .to_string_lossy()
                .into_owned(),
            MANIFEST.to_vec(),
            "default",
        )?;
        assert!(frontend.session_capability().persistent);
        let snapshot = frontend.discover()?;
        let key = session_key(&frontend, snapshot.manifest());
        let extraction = frontend.extract(&snapshot, key)?;
        assert_native_records(&extraction);
        Ok(())
    }

    #[test]
    fn unavailable_and_unsupported_never_synthesize_facts() -> Result<(), Box<dyn Error>> {
        let manifest_only =
            RustFrontend::new(SOURCE.to_vec(), TOOLCHAIN, MANIFEST.to_vec(), "default")?;
        let snapshot = manifest_only.discover()?;
        let key = session_key(&manifest_only, snapshot.manifest());
        let extraction = manifest_only.extract(&snapshot, key)?;
        assert!(matches!(
            extraction.coverage().state(),
            Coverage::Unsupported
        ));
        assert!(extraction.records().is_empty());

        let missing = RustFrontend::with_helper(
            SOURCE.to_vec(),
            TOOLCHAIN,
            std::env::temp_dir()
                .join(format!(
                    "backend-native-helper-that-does-not-exist-{}",
                    std::process::id()
                ))
                .to_string_lossy()
                .into_owned(),
            MANIFEST.to_vec(),
            "default",
        )?;
        let missing_snapshot = missing.discover()?;
        let missing_key = session_key(&missing, missing_snapshot.manifest());
        let extraction = missing.extract(&missing_snapshot, missing_key)?;
        assert!(matches!(
            extraction.coverage().state(),
            Coverage::Unavailable
        ));
        assert!(extraction.records().is_empty());
        Ok(())
    }

    #[test]
    fn stale_session_and_manifest_inputs_are_rejected_before_spawn() -> Result<(), Box<dyn Error>> {
        let helper = fixture("native_authority.py")
            .to_string_lossy()
            .into_owned();
        let frontend = RustFrontend::with_helper(
            SOURCE.to_vec(),
            TOOLCHAIN,
            helper.clone(),
            MANIFEST.to_vec(),
            "default",
        )?;
        let snapshot = frontend.discover()?;
        let key = session_key(&frontend, snapshot.manifest());
        let stale = RustFrontend::with_helper(
            SOURCE.to_vec(),
            TOOLCHAIN,
            fixture("stale_session_authority.py")
                .to_string_lossy()
                .into_owned(),
            MANIFEST.to_vec(),
            "default",
        )?;
        let stale_snapshot = stale.discover()?;
        let stale_key = session_key(&stale, stale_snapshot.manifest());
        assert!(matches!(
            stale.extract(&stale_snapshot, stale_key),
            Err(AuthorityError::Extraction(message))
                if message.contains("authority does not match")
        ));

        let changed = RustFrontend::with_helper(
            b"fn changed() {}".to_vec(),
            TOOLCHAIN,
            helper,
            MANIFEST.to_vec(),
            "default",
        )?;
        assert!(matches!(
            changed.extract(&snapshot, key),
            Err(AuthorityError::Extraction(message))
                if message.contains("snapshot does not match")
        ));
        Ok(())
    }

    #[test]
    fn response_language_revision_and_contract_bindings_are_checked() -> Result<(), Box<dyn Error>>
    {
        let language = RustFrontend::with_helper(
            SOURCE.to_vec(),
            TOOLCHAIN,
            fixture("wrong_language_authority.py")
                .to_string_lossy()
                .into_owned(),
            MANIFEST.to_vec(),
            "default",
        )?;
        let language_snapshot = language.discover()?;
        let language_key = session_key(&language, language_snapshot.manifest());
        assert!(matches!(
            language.extract(&language_snapshot, language_key),
            Err(AuthorityError::Extraction(message))
                if message.contains("authority does not match")
        ));

        let helper = fixture("native_authority.py")
            .to_string_lossy()
            .into_owned();
        let cold = RustFrontend::with_helper(
            SOURCE.to_vec(),
            TOOLCHAIN,
            helper.clone(),
            MANIFEST.to_vec(),
            "default",
        )?;
        let snapshot = cold.discover()?;
        let key = session_key(&cold, snapshot.manifest());
        let revised = DiscoverySnapshot::new(snapshot.manifest().clone(), 1);
        assert!(matches!(
            cold.extract(&revised, key),
            Err(AuthorityError::Extraction(message))
                if message.contains("authority does not match")
        ));

        let persistent = RustFrontend::with_persistent_helper(
            SOURCE.to_vec(),
            TOOLCHAIN,
            helper,
            MANIFEST.to_vec(),
            "default",
        )?;
        let persistent_snapshot = persistent.discover()?;
        assert_eq!(
            snapshot.manifest().digest(),
            persistent_snapshot.manifest().digest()
        );
        assert_ne!(cold.identity(), persistent.identity());
        assert!(matches!(
            persistent.extract(&persistent_snapshot, key),
            Err(AuthorityError::InvalidSessionKey)
        ));
        Ok(())
    }

    #[test]
    fn partial_payload_cannot_claim_complete_scope() -> Result<(), Box<dyn Error>> {
        let frontend = RustFrontend::with_helper(
            SOURCE.to_vec(),
            TOOLCHAIN,
            fixture("partial_authority.py")
                .to_string_lossy()
                .into_owned(),
            MANIFEST.to_vec(),
            "default",
        )?;
        let snapshot = frontend.discover()?;
        let key = session_key(&frontend, snapshot.manifest());
        let extraction = frontend.extract(&snapshot, key)?;
        assert!(matches!(extraction.coverage().state(), Coverage::Partial));
        assert_eq!(extraction.records().len(), 2);
        assert!(
            extraction
                .records()
                .iter()
                .all(|record| record.evidence().is_some())
        );
        Ok(())
    }

    #[test]
    fn native_process_failures_are_classified_without_facts() -> Result<(), Box<dyn Error>> {
        for name in ["failing_authority.py", "crashing_authority.py"] {
            let frontend = RustFrontend::with_helper(
                SOURCE.to_vec(),
                TOOLCHAIN,
                fixture(name).to_string_lossy().into_owned(),
                MANIFEST.to_vec(),
                "default",
            )?;
            let snapshot = frontend.discover()?;
            let key = session_key(&frontend, snapshot.manifest());
            assert!(matches!(
                frontend.extract(&snapshot, key),
                Err(AuthorityError::Extraction(message))
                    if message.contains("exited unsuccessfully")
            ));
        }
        Ok(())
    }

    #[test]
    fn malformed_deadline_and_output_are_rejected() -> Result<(), Box<dyn Error>> {
        let malformed = RustFrontend::with_helper(
            SOURCE.to_vec(),
            TOOLCHAIN,
            fixture("malformed_authority.py")
                .to_string_lossy()
                .into_owned(),
            MANIFEST.to_vec(),
            "default",
        )?;
        let malformed_snapshot = malformed.discover()?;
        let malformed_key = session_key(&malformed, malformed_snapshot.manifest());
        assert!(matches!(
            malformed.extract(&malformed_snapshot, malformed_key),
            Err(AuthorityError::Extraction(message))
                if message.contains("malformed native authority output")
        ));

        let deadline_limits = ProcessLimits::new(1024, 1024, Duration::from_millis(100), 4096)?;
        let slow = RustFrontend::with_helper_and_limits(
            SOURCE.to_vec(),
            TOOLCHAIN,
            fixture("slow_authority.py").to_string_lossy().into_owned(),
            MANIFEST.to_vec(),
            "default",
            deadline_limits,
        )?;
        let slow_snapshot = slow.discover()?;
        let slow_key = session_key(&slow, slow_snapshot.manifest());
        assert!(matches!(
            slow.extract(&slow_snapshot, slow_key),
            Err(AuthorityError::Process(ProcessError::Deadline))
        ));

        let output_limits = ProcessLimits::new(1024, 1024, Duration::from_secs(1), 4096)?;
        let noisy = RustFrontend::with_helper_and_limits(
            SOURCE.to_vec(),
            TOOLCHAIN,
            fixture("noisy_authority.py").to_string_lossy().into_owned(),
            MANIFEST.to_vec(),
            "default",
            output_limits,
        )?;
        let noisy_snapshot = noisy.discover()?;
        let noisy_key = session_key(&noisy, noisy_snapshot.manifest());
        let noisy_result = noisy.extract(&noisy_snapshot, noisy_key);
        assert!(matches!(
            noisy_result,
            Err(AuthorityError::Process(ProcessError::OutputLimit))
        ));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn replacing_same_path_toolchain_or_helper_changes_fences() -> Result<(), Box<dyn Error>> {
        let root =
            std::env::temp_dir().join(format!("backend-native-rust-fence-{}", std::process::id()));
        let _ = fs::create_dir(&root);
        let toolchain = root.join("toolchain");
        let helper = root.join("helper.py");
        fs::copy("/bin/sh", &toolchain)?;
        fs::copy(fixture("native_authority.py"), &helper)?;
        fs::set_permissions(&toolchain, fs::Permissions::from_mode(0o755))?;
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o755))?;

        let first = RustFrontend::with_helper(
            SOURCE.to_vec(),
            toolchain.to_string_lossy().into_owned(),
            helper.to_string_lossy().into_owned(),
            MANIFEST.to_vec(),
            "default",
        )?;
        let first_snapshot = first.discover()?;
        let first_key = session_key(&first, first_snapshot.manifest());
        let first_identity = first.identity();

        fs::write(&toolchain, b"replacement toolchain bytes")?;
        let second = RustFrontend::with_helper(
            SOURCE.to_vec(),
            toolchain.to_string_lossy().into_owned(),
            helper.to_string_lossy().into_owned(),
            MANIFEST.to_vec(),
            "default",
        )?;
        let second_snapshot = second.discover()?;
        let second_key = session_key(&second, second_snapshot.manifest());
        assert_ne!(first_identity, second.identity());
        assert_ne!(
            first_snapshot.manifest().digest(),
            second_snapshot.manifest().digest()
        );
        assert_ne!(first_key, second_key);

        fs::write(&helper, b"#!/bin/sh\nprintf malformed\n")?;
        let third = RustFrontend::with_helper(
            SOURCE.to_vec(),
            toolchain.to_string_lossy().into_owned(),
            helper.to_string_lossy().into_owned(),
            MANIFEST.to_vec(),
            "default",
        )?;
        let third_snapshot = third.discover()?;
        let third_key = session_key(&third, third_snapshot.manifest());
        assert_ne!(
            second_snapshot.manifest().digest(),
            third_snapshot.manifest().digest()
        );
        assert_ne!(second_key, third_key);
        assert!(matches!(
            third.extract(&first_snapshot, first_key),
            Err(AuthorityError::Extraction(message))
                if message.contains("snapshot does not match")
        ));

        let _ = fs::remove_file(&toolchain);
        let _ = fs::remove_file(&helper);
        let _ = fs::remove_dir(&root);
        Ok(())
    }
}
