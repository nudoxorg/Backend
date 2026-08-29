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
