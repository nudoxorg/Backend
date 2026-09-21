//! Durable, bounded remote package acquisition.
//!
//! This module is the single acquisition authority for the workspace. It owns
//! the one contract shared by every ecosystem: an admitted source endpoint, an
//! exact pinned package coordinate, a bounded metadata page and archive fetch,
//! content-address verification, and an atomically committed receipt. No other
//! crate defines a second registry transport, cursor, or cache.
//!
//! A registry is an external effect, never an authority over the workspace
//! head.  This module durably records an intent before network I/O, stores
//! content-addressed archive bytes beneath a deterministic per-ecosystem root
//! ([`storage_root`]), then commits the feed cursor and exact publication
//! receipt in one synced journal record.
//!
//! Availability is explicit and typed. [`AcquisitionOutcome::Offline`] is
//! returned without any network effect when local-first policy is configured,
//! [`AcquisitionOutcome::Unavailable`] is a durable retryable terminal, and
//! [`AcquisitionOutcome::RetryAfter`] carries a bounded server delay. A source
//! extent that exceeds a configured cap produces a measured, typed
//! [`AcquisitionError::Overrun`] or [`TransportFailure::Overrun`] instead of a
//! panic or a silent skip.

mod ecosystem;
mod facts;
mod feed;
mod identity;
mod owner;
mod router;
mod transport;
mod wire;

#[cfg(test)]
mod tests;

pub use ecosystem::{
    ChecksumAlgorithm, EcosystemAdapter, NativeArtifact, NativeArtifactKind, NativeDistTag,
    NativeFeature, NativeRelease, RegistryChecksum,
};
pub use facts::{DownloadCount, DownloadCountGap, ReleaseFacts, ReleaseStanding, SecurityStanding};
/// Preferred closed name for the seven native registry grammars.
pub use identity::RegistryEcosystem as RegistryKind;
pub use identity::{
    admit_registry_coordinate, AcquisitionLimits, AcquisitionPolicy, AuthenticationToken,
    CanonicalFeedV1, FeedCursor, FeedSchema, PackageCoordinate, PackageName, PackageVersion,
    ProvenanceDigest, PublishedArtifactClaim, RegistryCoordinate, RegistryEcosystem,
    RegistryEndpoint, RegistryId, RemoteRegistry,
};
pub use owner::{
    storage_root, AcquisitionError, AcquisitionIntent, AcquisitionOutcome, AcquisitionReceipt,
    PublishedPackage, RegistryOwner, RegistryReadiness, RegistryRecovery,
};
pub use router::{
    RegistryConfigurationError, RegistryRoute, RegistrySource, RegistrySourceSet,
    CARGO_SPARSE_INDEX, CONAN_CENTER, GO_MODULE_PROXY, MAVEN_CENTRAL, NPM_REGISTRY, NUGET_V3,
    PYPI_SIMPLE_API, REGISTRY_SOURCE_ROOT_VERSION,
};
pub use transport::{
    ArchiveArtifact, FeedPage, FeedRequest, HttpRegistryTransport, RegistryTransport,
    RemotePackage, TransportFailure, TransportResult,
};
