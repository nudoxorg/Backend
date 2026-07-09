use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum JavaError {
    #[error(transparent)]
    Package(#[from] JavaPackageError),
}

/// Project discovery, manifest, and oracle-extraction failures for a Java
/// package (the unit that becomes one `Index`).
///
/// Go-style explicit taxonomy (no stringy bail!); carries PathBufs,
/// captured tool stderr/stdout, project roots, and #[source] chains for
/// io/serde.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum JavaPackageError {
    #[error("{path} is not a directory")]
    NotDirectory { path: PathBuf },

    /// Maven layout detected but the pom disappeared or was unreadable
    /// before coordinate scan.
    #[error("no pom.xml found at expected location {path:?}")]
    NoPom { path: PathBuf },

    #[error("no Java source roots found under {root}")]
    NoSourceRoots { root: PathBuf },

    #[error("failed to read pom.xml at {path}")]
    PomReadFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Gradle or plain layout had no usable source roots after scan
    /// (covers multi-module conventional src/main/java etc.).
    #[error("Maven/Gradle/Plain project at {root} has no discoverable Java sources")]
    NoSourceRootsDetailed {
        root: PathBuf,
        layout: String,
    },

    #[error("extracting Java project at {root} failed")]
    OracleExtractFailed {
        root: PathBuf,
        #[source]
        source: OracleError,
    },

    /// Catch-all for version resolution inside traversal when wired.
    #[error(transparent)]
    Version(#[from] MavenVersionError),
}

/// Sub-enum for everything that happens inside `oracle::extract` /
/// `compile_oracle` / `run_doclet` (materialize, javac the doclet,
/// javadoc -doclet, argfile, json out).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum OracleError {
    #[error("no .java sources found under {roots:?}")]
    NoJavaSources { roots: Vec<PathBuf> },

    #[error(transparent)]
    Doclet(#[from] DocletError),

    #[error(transparent)]
    Javadoc(#[from] JavadocError),

    #[error("parsing oracle JSON output failed")]
    JsonParse(#[from] serde_json::Error),

    #[error(transparent)]
    Extraction(#[from] ExtractionError),
}

/// Failures while materializing + `javac` compiling the embedded
/// nudox.oracle.Extractor + Json (from the vendored Java doclet sources).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DocletError {
    #[error("failed creating classes directory {path:?}")]
    CreateClassesDirFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("spawning `javac` failed — is a JDK (17+, ideally 23+) on PATH? e.g. `nix shell nixpkgs#jdk`")]
    SpawnJavacFailed {
        #[source]
        source: std::io::Error,
    },

    #[error("compiling the javadoc doclet failed ({status}): {stderr}")]
    DocletCompileFailed {
        status: String,
        stderr: String,
        stdout: Option<String>,
    },

    #[error("failed writing .compiled stamp {path:?}")]
    WriteStampFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed writing oracle source file {path:?}")]
    MaterializeSourceFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed creating parent dir for oracle source {path:?}")]
    CreateMaterializeParentFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Failures while spawning/running `javadoc -doclet nudox.oracle.Extractor`,
/// writing @argfile, reading its JSON, etc. Mirrors patterns from the
/// Extractor (TypeMirror walks, extraction root, positions, etc.) plus
/// tool failures.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum JavadocError {
    #[error("failed writing javadoc @argfile {path:?}")]
    WriteArgfileFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("spawning `javadoc` failed — is a JDK (17+, ideally 23+ for Markdown doc comments) on PATH? e.g. `nix shell nixpkgs#jdk`")]
    SpawnJavadocFailed {
        #[source]
        source: std::io::Error,
    },

    #[error("javadoc oracle failed ({status}): {stderr}")]
    JavadocOracleFailed {
        status: String,
        stderr: String,
        stdout: Option<String>,
    },

    #[error("reading oracle output {path:?} (doclet ran but wrote nothing?) failed")]
    ReadOracleOutputFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("creating per-run tempdir {path:?} failed")]
    TempDirFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Sub-enum for post-deser validation / shape problems in the
/// Extraction (mirrors TypeMirror/Directive/Module/decl kinds from the
/// Java oracle/Extractor.java).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ExtractionError {
    #[error("oracle extraction format {format} unsupported (expected 1)")]
    UnsupportedFormat { format: u32 },

    #[error("oracle produced zero TypeDecls (no API surface under sources)")]
    NoTypesExtracted,

    #[error("extraction contained a TypeMirror::Error for unresolvable name `{name}` in API position")]
    TypeMirrorErrorInApi { name: String },

    #[error("unexpected kind `{kind}` for top-level type `{qname}`")]
    UnexpectedTypeKind { kind: String, qname: String },
}

/// Maven version request parse failures (from traversal, included for
/// completeness of "Maven issues").
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MavenVersionError {
    #[error("numeric segment overflows in Maven version request `{requested}`")]
    NumericSegmentOverflow {
        requested: String,
        #[source]
        source: std::num::ParseIntError,
    },

    #[error("unparseable Maven version request `{requested}`")]
    UnparseableVersion { requested: String },
}
