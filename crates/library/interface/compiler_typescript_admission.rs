//! Bounded, path-free facts for installed TypeScript compiler admission.
use super::TypeScriptAuthorityFailureKind;
use serde::{Deserialize, Deserializer, Serialize};

/// Validated package version, without paths, control characters or native text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct TypeScriptCompilerVersion(Box<str>);

impl TypeScriptCompilerVersion {
    /// Maximum admitted version text in the public failure facts.
    pub const MAX_BYTES: usize = 128;

    /// Admits the same bounded version grammar as the installed compiler host.
    #[must_use]
    pub fn new(version: &str) -> Option<Self> {
        if version.is_empty() || version.len() > Self::MAX_BYTES {
            return None;
        }
        let identifiers = |value: &str| {
            !value.is_empty()
                && value.split('.').all(|part| {
                    !part.is_empty()
                        && part
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                })
        };
        let (without_build, build) = version
            .split_once('+')
            .map_or((version, None), |(a, b)| (a, Some(b)));
        if build.is_some_and(|value| !identifiers(value)) {
            return None;
        }
        let (core, prerelease) = without_build
            .split_once('-')
            .map_or((without_build, None), |(a, b)| (a, Some(b)));
        if prerelease.is_some_and(|value| !identifiers(value)) {
            return None;
        }
        let mut parts = core.split('.');
        for _ in 0..3 {
            let part = parts.next()?;
            if part.is_empty()
                || !part.bytes().all(|byte| byte.is_ascii_digit())
                || (part.len() > 1 && part.starts_with('0'))
                || part.parse::<u64>().is_err()
            {
                return None;
            }
        }
        parts.next().is_none().then(|| Self(version.into()))
    }

    /// Exact admitted package version.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for TypeScriptCompilerVersion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let version = String::deserialize(deserializer)?;
        Self::new(&version)
            .ok_or_else(|| serde::de::Error::custom("invalid bounded TypeScript package version"))
    }
}

/// Which exact package selection encountered the admission refusal.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TypeScriptCompilerPackageRole {
    /// The package chosen to provide the compiler API.
    SelectedCompiler,
    /// A separately witnessed project package when another compiler was selected.
    DiscoveredProjectPackage,
    /// A package scan without retained selection context.
    Unspecified,
}

/// Required exports checked by the genuine compiler API bridge.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TypeScriptCompilerApiExport {
    /// Usage-site import/require resolution mode.
    GetModeForUsageLocation,
    /// Effective script target from compiler options.
    GetEmitScriptTarget,
    /// Effective module kind from compiler options.
    GetEmitModuleKind,
    /// Effective module resolution strategy.
    GetEmitModuleResolutionKind,
    /// Effective module detection strategy.
    GetEmitModuleDetectionKind,
    /// Effective ECMAScript module interop option.
    GetESModuleInterop,
    /// Effective synthetic default import option.
    GetAllowSyntheticDefaultImports,
    /// Compiler-owned bounded directory matcher.
    MatchFiles,
}

impl TypeScriptCompilerApiExport {
    /// Exact known export, never an arbitrary name from compiler output.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "getModeForUsageLocation" => Self::GetModeForUsageLocation,
            "getEmitScriptTarget" => Self::GetEmitScriptTarget,
            "getEmitModuleKind" => Self::GetEmitModuleKind,
            "getEmitModuleResolutionKind" => Self::GetEmitModuleResolutionKind,
            "getEmitModuleDetectionKind" => Self::GetEmitModuleDetectionKind,
            "getESModuleInterop" => Self::GetESModuleInterop,
            "getAllowSyntheticDefaultImports" => Self::GetAllowSyntheticDefaultImports,
            "matchFiles" => Self::MatchFiles,
            _ => return None,
        })
    }

    /// Compiler API spelling of this required export.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::GetModeForUsageLocation => "getModeForUsageLocation",
            Self::GetEmitScriptTarget => "getEmitScriptTarget",
            Self::GetEmitModuleKind => "getEmitModuleKind",
            Self::GetEmitModuleResolutionKind => "getEmitModuleResolutionKind",
            Self::GetEmitModuleDetectionKind => "getEmitModuleDetectionKind",
            Self::GetESModuleInterop => "getESModuleInterop",
            Self::GetAllowSyntheticDefaultImports => "getAllowSyntheticDefaultImports",
            Self::MatchFiles => "matchFiles",
        }
    }
}

/// Closed compiler-package admission facts; counts are observations, not totals.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TypeScriptCompilerAdmissionCause {
    /// Admitted runtime files exceeded the package file bound.
    RuntimeFiles {
        /// Runtime files observed before the scan stopped.
        observed: u64,
        /// Admitted runtime file limit.
        maximum: u64,
    },
    /// Visited filesystem entries exceeded the package traversal bound.
    FilesystemEntries {
        /// Filesystem entries visited before the scan stopped.
        observed: u64,
        /// Admitted traversal entry limit.
        maximum: u64,
    },
    /// A visited directory exceeded the package depth bound.
    DirectoryDepth {
        /// Depth of the refused directory.
        observed: u64,
        /// Admitted traversal depth limit.
        maximum: u64,
    },
    /// One captured compiler input exceeded its remaining byte allowance.
    InputBytes {
        /// Exact refused input byte length.
        observed: u64,
        /// Remaining admitted input byte allowance.
        maximum: u64,
    },
    /// The selected package lacks its JavaScript compiler API entrypoint.
    MissingApiEntry,
    /// The selected JavaScript compiler API lacks a known required export.
    MissingApiExport {
        /// Known required export missing from the selected API.
        export: TypeScriptCompilerApiExport,
    },
}

/// Safe selected-package evidence retained through storage and all presenters.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TypeScriptCompilerAdmissionDetail {
    schema_version: u8,
    role: TypeScriptCompilerPackageRole,
    version: Option<TypeScriptCompilerVersion>,
    cause: TypeScriptCompilerAdmissionCause,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TypeScriptCompilerAdmissionWire {
    schema_version: u8,
    role: TypeScriptCompilerPackageRole,
    version: Option<TypeScriptCompilerVersion>,
    cause: TypeScriptCompilerAdmissionCause,
}

impl<'de> Deserialize<'de> for TypeScriptCompilerAdmissionDetail {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = TypeScriptCompilerAdmissionWire::deserialize(deserializer)?;
        if wire.schema_version != Self::SCHEMA_VERSION {
            return Err(serde::de::Error::custom(
                "unsupported TypeScript compiler admission schema",
            ));
        }
        Self::new(wire.role, wire.version, wire.cause)
            .ok_or_else(|| serde::de::Error::custom("invalid TypeScript compiler admission facts"))
    }
}

impl TypeScriptCompilerAdmissionDetail {
    /// Closed wire schema shared by constructor and deserializer.
    pub const SCHEMA_VERSION: u8 = 1;

    /// Admits count semantics and selected-compiler/version/API pairing once.
    /// Returns `None` for a non-exceeded limit or missing API selection proof.
    #[must_use]
    pub fn new(
        role: TypeScriptCompilerPackageRole,
        version: Option<TypeScriptCompilerVersion>,
        cause: TypeScriptCompilerAdmissionCause,
    ) -> Option<Self> {
        use TypeScriptCompilerAdmissionCause as C;
        let admitted = match cause {
            C::RuntimeFiles { observed, maximum }
            | C::FilesystemEntries { observed, maximum }
            | C::DirectoryDepth { observed, maximum }
            | C::InputBytes { observed, maximum } => observed > maximum,
            C::MissingApiEntry | C::MissingApiExport { .. } => {
                version.is_some() && matches!(role, TypeScriptCompilerPackageRole::SelectedCompiler)
            }
        };
        admitted.then_some(Self {
            schema_version: Self::SCHEMA_VERSION,
            role,
            version,
            cause,
        })
    }

    /// Closed relationship to the selected compiler.
    #[must_use]
    pub const fn role(&self) -> TypeScriptCompilerPackageRole {
        self.role
    }

    /// Exact validated version when retained by host selection.
    #[must_use]
    pub fn version(&self) -> Option<&TypeScriptCompilerVersion> {
        self.version.as_ref()
    }

    /// Exact bounded limit or missing compiler API capability.
    #[must_use]
    pub const fn cause(&self) -> TypeScriptCompilerAdmissionCause {
        self.cause
    }

    /// Existing authority kind implied by these additional facts.
    #[must_use]
    pub const fn failure_kind(&self) -> TypeScriptAuthorityFailureKind {
        use TypeScriptCompilerAdmissionCause as C;
        match self.cause {
            C::RuntimeFiles { .. } | C::FilesystemEntries { .. } | C::DirectoryDepth { .. } => {
                TypeScriptAuthorityFailureKind::HostModuleFileLimit
            }
            C::InputBytes { .. } => TypeScriptAuthorityFailureKind::HostFileTooLarge,
            C::MissingApiEntry | C::MissingApiExport { .. } => {
                TypeScriptAuthorityFailureKind::HostCompilerApiBridge
            }
        }
    }

    /// Shared explanation from closed facts, with no filesystem paths or raw text.
    #[must_use]
    pub fn detail(&self) -> String {
        use TypeScriptCompilerAdmissionCause as C;
        let role = match self.role {
            TypeScriptCompilerPackageRole::SelectedCompiler => {
                "Selected TypeScript package typescript"
            }
            TypeScriptCompilerPackageRole::DiscoveredProjectPackage => {
                "Discovered TypeScript package typescript"
            }
            TypeScriptCompilerPackageRole::Unspecified => "TypeScript package typescript",
        };
        let version = self
            .version
            .as_ref()
            .map_or("version unavailable", TypeScriptCompilerVersion::as_str);
        let issue = match self.cause {
            C::RuntimeFiles { observed, maximum } => {
                format!("contains at least {observed} runtime files; limit {maximum}")
            }
            C::FilesystemEntries { observed, maximum } => {
                format!("visited at least {observed} filesystem entries; limit {maximum}")
            }
            C::DirectoryDepth { observed, maximum } => {
                format!("reached directory depth {observed}; limit {maximum}")
            }
            C::InputBytes { observed, maximum } => {
                format!("input has {observed} bytes; remaining input allowance {maximum}")
            }
            C::MissingApiEntry => {
                "has no JavaScript Compiler API entrypoint lib/typescript.js".to_owned()
            }
            C::MissingApiExport { export } => format!(
                "lacks required JavaScript Compiler API export {}",
                export.name()
            ),
        };
        let recovery = match self.cause {
            C::RuntimeFiles { .. }
            | C::FilesystemEntries { .. }
            | C::DirectoryDepth { .. }
            | C::InputBytes { .. } => {
                "Restore an intact compiler package within the stated bound before retrying."
            }
            C::MissingApiEntry | C::MissingApiExport { .. } => {
                "Select a JavaScript Compiler API package, such as TypeScript 5.9.3, before retrying."
            }
        };
        format!("{role} {version}: {issue}. {recovery}")
    }
}
