//! Durable, bounded remote package acquisition.
//!
//! A registry is an external effect, never an authority over the workspace
//! head.  This module durably records an intent before network I/O, stores
//! content-addressed archive bytes, then commits the feed cursor and exact
//! publication receipt in one synced journal record.

mod ecosystem;
mod feed;
mod identity;
mod owner;
mod transport;
mod wire;

#[cfg(test)]
mod tests;

pub use ecosystem::{ChecksumAlgorithm, EcosystemAdapter, NativeRelease, RegistryChecksum};
/// Preferred closed name for the seven native registry grammars.
pub use identity::RegistryEcosystem as RegistryKind;
pub use identity::{
    AcquisitionLimits, AcquisitionPolicy, AuthenticationToken, CanonicalFeedV1, FeedCursor,
    FeedSchema, PackageCoordinate, PackageName, PackageVersion, ProvenanceDigest,
    PublishedArtifactClaim, RegistryCoordinate, RegistryEcosystem, RegistryEndpoint, RegistryId,
    RemoteRegistry, admit_registry_coordinate,
};
pub use owner::{
    AcquisitionError, AcquisitionIntent, AcquisitionOutcome, AcquisitionReceipt, PublishedPackage,
    RegistryOwner, RegistryReadiness, RegistryRecovery,
};
pub use transport::{
    FeedPage, FeedRequest, HttpRegistryTransport, RegistryTransport, RemotePackage,
    TransportFailure, TransportResult,
};
