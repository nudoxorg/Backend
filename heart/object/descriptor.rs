//! Defines descriptor behavior for `heart-object`, whose purpose is to describe canonical objects, providers, and residency.
//! This module owns the descriptor invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::{
    cmp::Ordering,
    hash::{Hash, Hasher},
    ops::Deref,
};

use heart_identity::ContentId;
use heart_schema::SchemaId;

/// Exact canonical byte length of an immutable object.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct ObjectLength(u64);

impl From<u64> for ObjectLength {
    fn from(bytes: u64) -> Self {
        Self(bytes)
    }
}

impl Deref for ObjectLength {
    type Target = u64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Opaque schema-interpreted object-kind tag.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct ObjectKind(u16);

impl From<u16> for ObjectKind {
    fn from(raw: u16) -> Self {
        Self(raw)
    }
}

impl Deref for ObjectKind {
    type Target = u16;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Copyable descriptor for immutable canonical bytes in `DomainTag`.
///
/// The fields are already validated immutable metadata, so they remain public
/// rather than forcing accessor boilerplate throughout planning and storage.
#[repr(C)]
#[derive(derive_more::Debug)]
pub struct ObjectRef<DomainTag> {
    /// Typed canonical content identity.
    pub content: ContentId<DomainTag>,
    /// Exact canonical byte length.
    pub length: ObjectLength,
    /// Closed schema which interprets the object bytes.
    pub schema: SchemaId,
    /// Opaque schema-interpreted kind tag.
    pub kind: ObjectKind,
}

impl<DomainTag> Copy for ObjectRef<DomainTag> {}

impl<DomainTag> Clone for ObjectRef<DomainTag> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<DomainTag> PartialEq for ObjectRef<DomainTag> {
    fn eq(&self, other: &Self) -> bool {
        self.content == other.content
            && self.length == other.length
            && self.schema == other.schema
            && self.kind == other.kind
    }
}

impl<DomainTag> Eq for ObjectRef<DomainTag> {}

impl<DomainTag> PartialOrd for ObjectRef<DomainTag> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<DomainTag> Ord for ObjectRef<DomainTag> {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.content, self.length, self.schema, self.kind).cmp(&(
            other.content,
            other.length,
            other.schema,
            other.kind,
        ))
    }
}

impl<DomainTag> Hash for ObjectRef<DomainTag> {
    fn hash<HasherState: Hasher>(&self, state: &mut HasherState) {
        self.content.hash(state);
        self.length.hash(state);
        self.schema.hash(state);
        self.kind.hash(state);
    }
}

#[cfg(test)]
mod tests {
    use core::mem::{align_of, size_of};

    use heart_identity::{ContentId, ObjectDomain};
    use heart_schema::SchemaId;

    use super::ObjectRef;

    #[test]
    fn descriptor_is_exactly_48_bytes() {
        let descriptor = ObjectRef::<ObjectDomain> {
            content: ContentId::from_digest([3; 32]),
            length: 99.into(),
            schema: SchemaId::Object,
            kind: 7.into(),
        };
        let copied = descriptor;
        let mut expected = [3; 32];
        expected[0] = 1;
        assert_eq!(copied.content.as_ref(), &expected);
        assert_eq!(*copied.length, 99);
        assert_eq!(size_of::<ObjectRef<ObjectDomain>>(), 48);
        assert_eq!(align_of::<ObjectRef<ObjectDomain>>(), 8);
    }
}
