//! Verified complete immutable object owners.

use std::{fmt, sync::Arc};

use backend_version::{
    ObjectKey as VersionObjectKey, ObjectVersion as VersionObjectVersion, Schema,
};

use crate::{AuthorityClaim, ImmutableObjectSchema};

/// A complete immutable object after canonical identity verification.
pub struct CompleteObject<T: Schema = ImmutableObjectSchema> {
    /// Logical object key.
    pub key: VersionObjectKey<T>,
    /// Complete object version.
    pub version: VersionObjectVersion<T>,
    /// Canonical object bytes.
    pub bytes: Vec<u8>,
    /// Untrusted authority claim carried with this object.
    pub authority: AuthorityClaim,
}
impl<T: Schema> Clone for CompleteObject<T> {
    fn clone(&self) -> Self {
        Self {
            key: self.key,
            version: self.version,
            bytes: self.bytes.clone(),
            authority: self.authority,
        }
    }
}
impl<T: Schema> fmt::Debug for CompleteObject<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompleteObject")
            .field("key", &self.key)
            .field("version", &self.version)
            .field("bytes", &self.bytes)
            .field("authority", &self.authority)
            .finish()
    }
}
impl<T: Schema> PartialEq for CompleteObject<T> {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
            && self.version == other.version
            && self.bytes == other.bytes
            && self.authority == other.authority
    }
}
impl<T: Schema> Eq for CompleteObject<T> {}

impl<T: Schema> CompleteObject<T> {
    /// Moves this accepted object's bytes into an immutable shared owner.
    ///
    /// No payload copy is made. This is the preferred handoff when multiple
    /// consumers retain the same canonical object.
    #[must_use]
    pub fn into_shared(self) -> SharedCompleteObject<T> {
        SharedCompleteObject {
            key: self.key,
            version: self.version,
            bytes: self.bytes.into(),
            authority: self.authority,
        }
    }
}

/// Compatibility name for a complete accepted object.
pub type AcceptedObject<T = ImmutableObjectSchema> = CompleteObject<T>;

/// A complete immutable object whose canonical bytes are reference counted.
///
/// Cloning this value shares the verified object allocation. The object still
/// carries the same typed identity and authority metadata as
/// [`CompleteObject`].
pub struct SharedCompleteObject<T: Schema = ImmutableObjectSchema> {
    /// Logical object key.
    pub key: VersionObjectKey<T>,
    /// Complete object version.
    pub version: VersionObjectVersion<T>,
    /// Canonical object bytes shared across owners.
    pub bytes: Arc<[u8]>,
    /// Untrusted authority claim carried with this object.
    pub authority: AuthorityClaim,
}
impl<T: Schema> Clone for SharedCompleteObject<T> {
    fn clone(&self) -> Self {
        Self {
            key: self.key,
            version: self.version,
            bytes: Arc::clone(&self.bytes),
            authority: self.authority,
        }
    }
}
impl<T: Schema> fmt::Debug for SharedCompleteObject<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SharedCompleteObject")
            .field("key", &self.key)
            .field("version", &self.version)
            .field("bytes", &self.bytes)
            .field("authority", &self.authority)
            .finish()
    }
}
impl<T: Schema> PartialEq for SharedCompleteObject<T> {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
            && self.version == other.version
            && self.bytes == other.bytes
            && self.authority == other.authority
    }
}
impl<T: Schema> Eq for SharedCompleteObject<T> {}
impl<T: Schema> SharedCompleteObject<T> {
    /// Returns the canonical bytes as a borrowed slice.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns a cheap shared handle to the canonical bytes.
    #[must_use]
    pub fn bytes_shared(&self) -> Arc<[u8]> {
        Arc::clone(&self.bytes)
    }
}
