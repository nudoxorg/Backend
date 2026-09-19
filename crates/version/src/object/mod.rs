//! The `backend_version::object` module describes canonical objects, providers, and residency.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
//! Compact immutable object descriptors and truthful locality vocabulary.

mod canonical;
mod descriptor;
mod provider;
mod residence;

pub use self::canonical::{
    OBJECT_DESCRIPTOR_RECORD_BYTES, ObjectDescriptorDecodeError, ObjectDescriptorOutputTooSmall,
    ObjectDescriptorWireRecord,
};
pub use self::descriptor::{ObjectKind, ObjectLength, ObjectRef};
pub use self::provider::{ProviderId, ProviderIdError, ProviderSet, ProviderSetError};
pub use self::residence::{DepSetId, RemoteBase};
