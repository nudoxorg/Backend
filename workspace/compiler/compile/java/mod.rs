pub mod context;
pub mod error;
pub mod function;
pub mod item;
pub mod javadoc;
pub mod oracle;
pub mod package;
pub mod schema;
pub mod traversal;
pub mod types;

// Reexports for callers (java::lower_package, java::JavaError, etc.)
pub use package::{
    discover_project, lower_package, parse_pom, JavaProject, Layout, MavenCoordinates, PomInfo,
};

pub use error::{
    JavaError, JavaPackageError, OracleError, DocletError, JavadocError, ExtractionError, MavenVersionError,
};
