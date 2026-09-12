//! The `interface-library` crate exists to own the one shared local library every surface reads, adds to, and searches.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
//!
//! The library is the engine behind the GUI, the CLI, and the MCP server. All three open the same
//! [`WorkspaceRoot`], see the same [`Shelf`], and observe each other's changes through the
//! [`LibraryEpoch`]. A package added from any one surface is visible to the other two on their next
//! read, without a daemon: durable state lives in the workspace, and every mutation bumps the
//! epoch file that readers watch.
//!
//! # Layout beneath the workspace root
//!
//! ```text
//! <root>/
//!   artifacts/   compiler-owned immutable publications
//!   journal/     compiler-owned publication journal
//!   library/
//!     shelf      requested packages, their status, and their publication locators
//!     epoch      monotone counter bumped after every shelf mutation
//!     lock       held for the duration of one compile so two processes never publish at once
//!     tantivy/   durable lexical projections keyed by segment identity
//!     catalog.db registry feed observations recorded by the acquire pipeline
//! ```
//!
//! # Invariants the types carry
//!
//! * Compiling needs an [`Admission`]: move-only, library-borrowing, consumed by `run`.
//! * Image bytes are only visible inside [`Library::read`]; what escapes is owned document data.
//! * Every symbol-bearing row carries an [`interface_identity::ExactAddress`] minted by a projector.
//! * All three surfaces dispatch through [`Library::execute`] over the closed [`Command`] enum.

mod add;
mod admission;
mod command;
mod compose;
mod compose_service;
mod epoch;
mod explore;
mod follow;
mod follow_service;
mod health;
mod image;
mod library;
mod page;
mod project;
mod project_service;
mod registry;
mod registry_service;
pub mod render;
mod session;
mod session_service;
mod shelf;
mod source;
mod source_service;
mod workspace;

pub use add::{
    AddFailure, AddOutcome, AddProgress, AddRejection, CompilePhaseProgress, RejectedAdd,
    RemoveOutcome,
};
pub use admission::Admission;
pub use command::{
    COMMANDS, Command, CommandId, CommandSpec, Domain, Mutation, Reply, package_name_of, spec,
    spec_named,
};
pub use compose::{
    DiffChange, DiffRequest, MAX_DIFF_SYMBOLS, MAX_READ_LOCATORS, PackageDiff, ReadPage,
    ReadRequest, ReadTerminal,
};
pub use epoch::{EpochError, EpochPhase, LibraryEpoch, LibraryWatcher};
pub use explore::{
    Cycle, ExploreCoverage, ExploreError, ExploreLimit, ExplorePackageName, ExplorePackageNameError,
    ExploreQuery, ExploreQueryError, ExploreUnavailable, FeedChecksum, IndexHitRow, IndexSearchPage,
    DEFAULT_EXPLORE_LIMIT, MAX_EXPLORE_LIMIT, MAX_EXPLORE_QUERY_BYTES, MAX_PACKAGE_NAME_BYTES,
    PackageProfile, PackageVersionRow, PackageVersionRows, PackageVersionText, VersionActive,
    checksum_hex,
};
pub use follow::{
    FollowError, FollowKey, FollowKeyError, FollowOutcome, FollowRequest, MAX_SUBSCRIPTIONS,
    Release, ReleaseFault, Releases, ReleasesRequest, Subscription, Subscriptions,
};
pub use health::{CAPABILITY_COUNT, Capability, CapabilityState, Health};
pub use project::{
    CreateProject, LockfileBinding, LockfileKind, MAX_PROJECT_MEMBERS, MAX_PROJECT_NAME_BYTES,
    MAX_PROJECTS, MemberChange, Project, ProjectError, ProjectHue, ProjectId, ProjectName,
    ProjectNameError, ProjectSelector, Projects, Repin, SyncCompile, SyncReport, SyncRequest,
};
pub use registry::{
    Dependency, DependencyKind, Dependents, DependentsPage, DependentsRequest, DetailRequest,
    DownloadCount, DownloadWindow, Downloads, ExplorePage, ExploreRequest, ExploreSort, Install,
    Links, MAX_EXPLORE_CARDS, MAX_OWNER_HANDLE_BYTES, MAX_PAGE_NUMBER, MAX_URL_BYTES, Origin,
    Owner, OwnerHandle, OwnerHandleError, OwnerKind, OwnerPage, OwnerRequest, PackageDetail,
    PageNumber, Provenance, Readme, ReadmeOrigin, RegistryCapability, RegistryCard, RegistryError,
    RegistryVersion, Url, UrlError, install_snippet,
};
pub use session::{
    MAX_TREE_NODES, Opener, SessionTree, TreeCloseRequest, TreeCloseScope, TreeError, TreeNode,
    TreeNodeId, TreeOpenRequest, TreeOutcome, TreeSubject, TreeSubjectError,
};
pub use source::{
    ContextLines, DEFAULT_CONTEXT_LINES, LineNumber, MAX_CONTEXT_LINES, MAX_SOURCE_BYTES, Related,
    RelatedRequest, RelatedRow, Relation, SourceError, SourceRequest, SourceText,
};
pub use image::Image;
pub use library::{CompilerAttachment, Library, LibraryOpenError, OpenOptions};
pub use page::{PageError, PageLocator, ReopenError, ReopenPhase, Resolution, ResolveError};
pub use shelf::{
    MAX_SHELF_ENTRIES, PackageCard, Shelf, ShelfEntry, ShelfError, ShelfFailure, ShelfStatus,
    Timestamp,
};
pub use workspace::{WorkspaceRoot, WorkspaceRootError};
