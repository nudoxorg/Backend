use core::{
    cmp::Ordering,
    hash::{Hash, Hasher},
};

use nudox_id::{ContentId, DependencySetDomain, GenerationId};

use crate::ObjectRef;

/// Exact remote state replaced by an overlay.
#[derive(derive_more::Debug)]
pub enum RemoteBase<DomainTag> {
    /// A specific object in a named remote generation.
    Present {
        /// Exact remote generation.
        generation: GenerationId,
        /// Object selected at this key in that generation.
        object: ObjectRef<DomainTag>,
    },
    /// The named remote generation had no entry at this key.
    Absent {
        /// Exact remote generation checked for this key.
        generation: GenerationId,
    },
}

impl<DomainTag> Copy for RemoteBase<DomainTag> {}
impl<DomainTag> Clone for RemoteBase<DomainTag> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<DomainTag> PartialEq for RemoteBase<DomainTag> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Present {
                    generation: left_generation,
                    object: left_object,
                },
                Self::Present {
                    generation: right_generation,
                    object: right_object,
                },
            ) => left_generation == right_generation && left_object == right_object,
            (Self::Absent { generation: left }, Self::Absent { generation: right }) => {
                left == right
            }
            _ => false,
        }
    }
}
impl<DomainTag> Eq for RemoteBase<DomainTag> {}
impl<DomainTag> PartialOrd for RemoteBase<DomainTag> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl<DomainTag> Ord for RemoteBase<DomainTag> {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Absent { generation: left }, Self::Absent { generation: right }) => {
                left.cmp(right)
            }
            (Self::Absent { .. }, Self::Present { .. }) => Ordering::Less,
            (Self::Present { .. }, Self::Absent { .. }) => Ordering::Greater,
            (
                Self::Present {
                    generation: left_generation,
                    object: left_object,
                },
                Self::Present {
                    generation: right_generation,
                    object: right_object,
                },
            ) => (left_generation, left_object).cmp(&(right_generation, right_object)),
        }
    }
}
impl<DomainTag> Hash for RemoteBase<DomainTag> {
    fn hash<HasherState: Hasher>(&self, state: &mut HasherState) {
        match self {
            Self::Present { generation, object } => {
                0_u8.hash(state);
                generation.hash(state);
                object.hash(state);
            }
            Self::Absent { generation } => {
                1_u8.hash(state);
                generation.hash(state);
            }
        }
    }
}
/// Typed identity of the exact descriptor closure verified before publication.
pub type DepSetId = ContentId<DependencySetDomain>;
