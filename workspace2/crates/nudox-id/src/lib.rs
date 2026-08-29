#![no_std]
#![forbid(unsafe_code)]
//! Typed, streamable BLAKE3 identities for canonical Nudox facts.
//!
//! Domain and encoding labels have fixed protocol width. Logical or encoded bytes are the final
//! BLAKE3 stream component. Logical content consumes declared canonical records; encoded artifacts
//! accept arbitrary borrowed chunks so mapped files, range reads, and network bodies need not be
//! staged before hashing.

#[cfg(test)]
extern crate std;

mod artifact;
mod content;
mod generation;
mod marker;
mod raw;

pub use artifact::{ArtifactHasher, ArtifactId};
pub use content::{ContentHasher, ContentId, ContentRoutingWord, FixedCanonicalRecord};
pub use generation::{GenerationHasher, GenerationId};
pub use marker::{
    CapabilityDomain, ConfigurationDomain, DependencySetDomain, Domain, DomainTag, Encoding,
    EncodingTag, FrameEncoding, LocalitySortedEncoding, ObjectDomain, ObjectPackEncoding,
    OperationDomain, RootDomain, StageKeyDomain,
};
use raw::{ARTIFACT_PERSONALIZATION, CONTENT_PERSONALIZATION};
pub use raw::{HASH_BYTES, TAG_BYTES};
