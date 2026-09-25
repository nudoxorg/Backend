//! Closed compiler facts shared by registries, native drivers, publication, and interfaces.
//! This crate describes requests and failures but deliberately performs no compilation or I/O.
//! Stable numeric conversions belong here because those values participate in canonical identities.

mod authority;
mod clang;
mod csharp;
mod go;
mod java;
mod package;
mod profile;
mod projection;
mod python;
mod recipe;
mod typescript;

pub use self::authority::{
    AuthorityDiagnosticClass, AuthorityPhase, InvalidUtf8Fact, Language, LanguageOracleTask,
    LoweringUnsupported, MAX_NATIVE_DIAGNOSTIC_BYTES, MAX_NATIVE_WORKER_PANIC_BYTES,
    NativeArtifactRole, NativeTool, NativeWorkPhase, NativeWorker, NativeWorkerPanic,
    NativeWorkerPanicClass, NativeWorkerPanicMessage, RegistryEcosystem, Stage,
    UnknownRegistryEcosystem,
};
pub use self::clang::{
    ClangProjectionDeclaration, ClangProjectionFault, ClangProjectionQualifiers,
    ClangProjectionTypeKind,
};
pub use self::csharp::{
    CSharpImageFault, CSharpImageHeaderFault, CSharpImageSection, CSharpImageTypeKind,
    CSharpProjectionFault, CSharpProjectionIndexPhase,
};
pub use self::go::{
    GoImageDeclarationKind, GoImageDocOwnerKind, GoImageFault, GoImageFlagCell, GoImageHeaderFault,
    GoImageMemberKind, GoImagePlane, GoImageTypeKind, GoProjectionFault, GoProjectionIndexPhase,
    GoProjectionListPhase,
};
pub use self::java::{
    JavaForeignKeyFault, JavaImageAtomFault, JavaImageFault, JavaImageHeaderFault, JavaImagePlane,
    JavaImageSectionFault, JavaProjectionFault, JavaProjectionIndexPhase, JavaProjectionTypeKind,
    JavaSymbolAtom,
};
pub use self::package::{
    MAX_PACKAGE_URL_BYTES, PackageTextRange, PackageType, PackageUrl, PackageUrlError,
    PackageUrlFacts, RejectedPackageUrl,
};
pub use self::profile::{
    CSharpVersion, CStandard, CxxStandard, GoVersion, JavaRelease, LanguageProfile, PythonVersion,
    RustEdition, TypeScriptSource, UnknownLanguageProfile,
};
pub use self::projection::{
    ProjectionAdmissionFault, ProjectionChildRole, ProjectionConstructorFault,
    ProjectionConstructorTag, ProjectionFactLane, ProjectionForeignKeyFault, ProjectionLineagePart,
    ProjectionPackageLineageFault, ProjectionParentageState, ProjectionSemanticTypeFault,
    ProjectionSemanticTypeTag, ProjectionSpan, ProjectionTypeCell, ProjectionTypeChildLane,
};
pub use self::python::PythonProjectionFault;
pub use self::recipe::{CompileRecipeFact, FrontendError};
pub use self::typescript::TypeScriptProjectionFault;
