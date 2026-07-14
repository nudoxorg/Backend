//! Explicit failure taxonomy for the C# producer (Go-style, no stringy
//! `bail!`); carries `PathBuf`s, captured tool stderr/stdout, and `#[source]`
//! chains. Mirrors `java::error`.

use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CSharpError {
	#[error(transparent)]
	Package(#[from] CSharpPackageError),
}

/// Project discovery, manifest, and oracle-extraction failures for a C#
/// package (the unit that becomes one `Index`).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CSharpPackageError {
	#[error("{path} is not a directory")]
	NotDirectory { path: PathBuf },

	/// SDK layout detected but the csproj disappeared before the scan.
	#[error("no .csproj found at expected location {path:?}")]
	NoProject { path: PathBuf },

	#[error("failed to read csproj at {path}")]
	ProjectReadFailed {
		path: PathBuf,
		#[source]
		source: std::io::Error,
	},

	/// No usable source roots (mode S) after scanning.
	#[error("C# project at {root} (layout {layout}) has no discoverable .cs sources")]
	NoSourceRoots { root: PathBuf, layout: String },

	#[error("extracting C# project at {root} failed")]
	OracleExtractFailed {
		root: PathBuf,
		#[source]
		source: OracleError,
	},

	/// Version resolution failure (from traversal, when wired).
	#[error(transparent)]
	Version(#[from] NuGetVersionError),
}

/// Everything that happens inside `oracle::extract`: resolving the published
/// oracle publish-dir, running `dotnet oracle.dll`, reading its JSON out.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum OracleError {
	#[error("no .cs sources found under {roots:?}")]
	NoSources { roots: Vec<PathBuf> },

	#[error(transparent)]
	Publish(#[from] PublishError),

	#[error(transparent)]
	Dotnet(#[from] DotnetError),

	#[error("parsing oracle JSON output failed")]
	JsonParse(#[from] serde_json::Error),

	#[error(transparent)]
	Extraction(#[from] ExtractionError),
}

/// Failures resolving the Buck2-built oracle publish dir
/// (`//workspace/compiler/compile/csharp/oracle:oracle`).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PublishError {
	#[error(
		"resolving the buck2-built C# oracle publish dir failed — was this binary built by buck2?"
	)]
	ResourceNotFound {
		#[source]
		source: std::io::Error,
	},

	#[error("the C# oracle publish dir {path:?} has no oracle.dll entrypoint")]
	MissingEntrypoint { path: PathBuf },
}

/// Failures while spawning / running `dotnet oracle.dll`, writing scratch,
/// or reading its JSON output.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DotnetError {
	#[error("creating per-run scratch dir {path:?} failed")]
	ScratchDirFailed {
		path: PathBuf,
		#[source]
		source: std::io::Error,
	},

	#[error(
		"spawning `dotnet` failed — is a .NET 10 SDK on PATH? e.g. add `dotnetCorePackages.sdk_10_0` to the devshell"
	)]
	SpawnDotnetFailed {
		#[source]
		source: std::io::Error,
	},

	#[error("C# oracle failed ({status}): {stderr}")]
	OracleFailed {
		status: String,
		stderr: String,
		stdout: Option<String>,
	},

	#[error("reading oracle output {path:?} (oracle ran but wrote nothing?) failed")]
	ReadOracleOutputFailed {
		path: PathBuf,
		#[source]
		source: std::io::Error,
	},
}

/// Post-deserialization validation / shape problems in the extraction.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ExtractionError {
	#[error("oracle extraction format {format} unsupported (expected 1)")]
	UnsupportedFormat { format: u32 },

	#[error("oracle produced zero type declarations (no API surface)")]
	NoTypesExtracted,
}

/// NuGet version request parse failures (from traversal).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum NuGetVersionError {
	#[error("numeric segment overflows in NuGet version request `{requested}`")]
	NumericSegmentOverflow {
		requested: String,
		#[source]
		source: std::num::ParseIntError,
	},

	#[error("unparseable NuGet version request `{requested}`")]
	UnparseableVersion { requested: String },
}
