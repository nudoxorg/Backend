//! C# (.NET / NuGet) producer: Roslyn oracle → IR (CSHARP-PLAN).

pub mod context;
pub mod error;
pub mod function;
pub mod item;
pub mod nupkg;
pub mod oracle;
pub mod package;
pub mod producer;
pub mod schema;
pub mod types;
pub mod xmldoc;

// Reexports for callers (csharp::lower_package, csharp::CSharpError, etc.)
pub use package::{CSharpProject, CsprojInfo, Layout, discover_project, lower_package, parse_csproj};
pub use producer::CSharpProducer;

pub use error::{
	CSharpError, CSharpPackageError, DotnetError, ExtractionError, NuGetVersionError, OracleError,
	PublishError,
};
