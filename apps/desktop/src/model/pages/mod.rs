//! Typed read models for every page the boards render.
//!
//! Each model is assembled once, off the UI thread, from the engine replies a
//! page needs, and is rich enough that a view never re-fetches to render it.
//! Every model lives in a keyed, LRU-bounded [`store::PageStore`] slot as a
//! [`crate::core::Resource`], so availability (not yet, working, loaded,
//! unavailable, fault) is explicit, and inside a loaded model every field that
//! the engine may not know is a [`common::Known`] with a typed gap.

pub mod common;
pub mod health;
pub mod key;
pub mod lines;
pub mod orbit;
pub mod package;
pub mod search;
pub(crate) mod serde_ext;
pub mod source;
pub mod store;
pub mod symbol;

pub use backend_library::Obligation;
pub use common::{
    ByteSpan, DeclFacts, DeclRef, Deprecation, Derivation, Gap, GapReason, KeyError, KindFamily,
    Known, LineSpan, PackageRef, Provenance, RowKey, SymbolRef, confidence_name, link_name,
};
pub use health::{FaultProgress, HealthModel, IngestModel, LanguageProgress, MissingCapability};
pub use key::{PageKey, SearchQuery};
pub use lines::{Resolution, UseLine};
pub use orbit::{
    IndexedPackage, OrbitModel, OrbitProject, Readiness, TreeNode, TreeOpener, TreeSubject,
};
pub use package::{
    AdvisorySummary, Dependency, DependencyScope, Downloads, OutlineNode, OutlineTree,
    PackageDossier, PackageRecord, RecordSource, Standing, VersionEntry,
};
pub use search::{MatchReason, SearchContinuation, SearchPage, SearchRow};
pub use source::{IdentifierSpan, SourceCoverage, SourceOrigin, SourceText, SourceView};
pub use store::{Capacity, Generation, Landing, PageStore, PageValue, ReadFailure, SeedEntry, Stamp};
pub use symbol::{
    Arrival, DocEntry, DocFragment, DocSection, DocSections, Excerpt, FileSpan, Member, Members,
    MembersCoverage, MethodGroup, NameLinkCoverage, OutlinePosition, SectionKind, Receiver,
    ReferenceScope, ReferenceSite, Relation, RelationKind, Rose, SignatureText, SignatureToken,
    SourceLocation, SourceSite, SymbolLink, SymbolPage, TokenClass,
};
