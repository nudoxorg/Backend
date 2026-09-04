//! Domain-separated, streamable identities for every canonical product fact and artifact.
//! The crate owns the closed marker registry and checked conversion from durable bytes.
//! Transparent typed IDs remain cheap to copy while making cross-domain substitution impossible.
#![no_std]
#![forbid(unsafe_code)]
//! Typed, streamable BLAKE3 identities for canonical product facts.
//!
//! Domain and encoding labels have fixed protocol width. Logical or encoded bytes are the final
//! BLAKE3 stream component. Logical content consumes declared canonical records; encoded artifacts
//! accept arbitrary borrowed chunks so mapped files, range reads, and network bodies need not be
//! staged before hashing.
//!
//! Content identities retain 248 digest bits (about a 124-bit birthday-collision budget), while
//! artifact identities retain 240 digest bits (about a 120-bit birthday-collision budget) after
//! their protocol authority cells.

#[cfg(test)]
extern crate std;

mod artifact;
mod authority;
mod content;
mod generation;
mod marker;
mod raw;

pub use artifact::{ArtifactHasher, ArtifactId, ArtifactIdDecodeError};
pub use authority::{ContentAuthority, ContentAuthorityError};
pub use content::{
    CONTENT_PAYLOAD_BYTES, ContentHasher, ContentId, ContentIdDecodeError, ContentRoutingWord,
    FixedCanonicalRecord,
};
pub use generation::{GenerationHasher, GenerationId};
pub use marker::{
    CapabilityDomain, CompilationTargetDomain, CompilePublicationDomain,
    CompilePublicationEncoding, CompileRecipeDomain, ConfigurationDomain, DependencySetDomain,
    Domain, DomainCode, DomainTag, Encoding, EncodingCode, EncodingTag, FrameEncoding,
    IndexExactSegmentDomain, IndexLexicalSegmentDomain, IndexPackDomain, IndexPackEncoding,
    IndexSnapshotDomain, IndexVectorSegmentDomain, IrFragmentDomain, IrFragmentEncoding,
    IrFragmentRangeEncoding, IrManifestDomain, IrManifestEncoding, IrSemanticImageDomain,
    IrSemanticImageEncoding, LocalitySortedEncoding,
    ObjectDomain, ObjectPackEncoding, OperationDomain, RootDomain, SourceFactDomain,
    StageKeyDomain, ToolchainDomain, DeclarationKeyDomain, DeclarationFamilyDomain,
    DeclarationVariantDomain, ForeignDeclarationDomain, SemanticScopeDomain,
};
use raw::{ARTIFACT_PERSONALIZATION, CONTENT_PERSONALIZATION};
pub use raw::{HASH_BYTES, TAG_BYTES};
