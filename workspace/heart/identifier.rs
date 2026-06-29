//! The shared, type-tagged global identifier used across every module.

use std::marker::PhantomData;

use uuid::Uuid;

/// Our struct for defining global UUIDs
pub struct Id<T>(Uuid, PhantomData<fn() -> T>);


impl<T> Clone for Id<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Id<T> {}
impl<T> PartialEq for Id<T> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}
impl<T> Eq for Id<T> {}
impl<T> std::hash::Hash for Id<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.hash(state)
    }
}
