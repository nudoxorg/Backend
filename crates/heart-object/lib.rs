//! The `heart-object` crate exists to describe canonical objects, providers, and residency.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
#![no_std]
#![forbid(unsafe_code)]
//! Compact immutable object descriptors and truthful locality vocabulary.

mod canonical;
mod descriptor;
mod provider;
mod residence;

pub use canonical::{
    OBJECT_DESCRIPTOR_RECORD_BYTES, ObjectDescriptorDecodeError, ObjectDescriptorOutputTooSmall,
    ObjectDescriptorWireRecord,
};
pub use descriptor::{ObjectKind, ObjectLength, ObjectRef};
pub use provider::{ProviderId, ProviderIdError, ProviderSet, ProviderSetError};
pub use residence::{DepSetId, RemoteBase};
