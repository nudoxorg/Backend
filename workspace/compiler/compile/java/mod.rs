pub mod context;
pub mod error;
pub mod function;
pub mod item;
pub mod javadoc;
pub mod oracle;
pub mod package;
pub mod producer;
pub mod schema;
pub mod traversal;
pub mod types;

// Reexports for callers (java::lower_package, java::JavaError, etc.)
pub use package::{
	JavaProject, Layout, MavenCoordinates, PomInfo, discover_project, lower_package, parse_pom,
};
pub use producer::JavaProducer;

pub use error::{
	DocletError, ExtractionError, JavaError, JavaPackageError, JavadocError, MavenVersionError,
	OracleError,
};
