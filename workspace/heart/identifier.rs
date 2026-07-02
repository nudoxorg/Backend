//! The shared, type-tagged global identifier used across every module.
//!
//! `Id<T>` is a UUID branded with the phantom type `T`, so an `Id<Symbol>` can
//! never be passed where an `Id<Package>` is expected. The tag is
//! variance-free (`fn() -> T`) so `Id<T>` is `Send`/`Sync`/`Copy` regardless of
//! `T`, and never borrows or drops a `T`.

use std::{fmt, marker::PhantomData};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A globally-unique, type-tagged identifier.
///
/// Construction is explicit and total:
/// - [`Id::from_uuid`] wraps a raw UUID (e.g. one read back from storage);
/// - [`Id::new_random`] mints a fresh v4 id for content with no natural key;
/// - [`Id::from_name`] derives a deterministic v5 id from a namespace + bytes,
///   which is how every *stable* identity in the system is produced.
pub struct Id<T>(Uuid, PhantomData<fn() -> T>);

impl<T> Id<T> {
	/// Wrap a raw UUID as a tagged id.
	pub const fn from_uuid(uuid: Uuid) -> Self { Self(uuid, PhantomData) }

	/// Mint a fresh random (v4) id. Use only for values with no natural,
	/// reproducible key; prefer [`Id::from_name`] for anything content-derived.
	pub fn new_random() -> Self { Self(Uuid::new_v4(), PhantomData) }

	/// Derive a deterministic (v5) id from a namespace and name bytes. The same
	/// inputs always yield the same id, on any machine, forever — this is the
	/// backbone of the global index's offline-recomputable identity.
	pub fn from_name(namespace: &Uuid, name: &[u8]) -> Self {
		Self(Uuid::new_v5(namespace, name), PhantomData)
	}

	/// The underlying raw UUID.
	pub const fn as_uuid(&self) -> &Uuid { &self.0 }

	/// Consume into the raw UUID.
	pub const fn into_uuid(self) -> Uuid { self.0 }

	/// Re-tag this id as identifying a different type. Deliberately explicit —
	/// the only sanctioned way to cross the phantom boundary.
	pub const fn cast<U>(self) -> Id<U> { Id(self.0, PhantomData) }
}

impl<T> Clone for Id<T> {
	fn clone(&self) -> Self { *self }
}
impl<T> Copy for Id<T> {}
impl<T> PartialEq for Id<T> {
	fn eq(&self, other: &Self) -> bool { self.0 == other.0 }
}
impl<T> Eq for Id<T> {}
impl<T> PartialOrd for Id<T> {
	fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> { Some(self.cmp(other)) }
}
impl<T> Ord for Id<T> {
	fn cmp(&self, other: &Self) -> std::cmp::Ordering { self.0.cmp(&other.0) }
}
impl<T> std::hash::Hash for Id<T> {
	fn hash<H: std::hash::Hasher>(&self, state: &mut H) { self.0.hash(state); }
}
impl<T> fmt::Debug for Id<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "Id<{}>({})", std::any::type_name::<T>(), self.0)
	}
}
impl<T> fmt::Display for Id<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { fmt::Display::fmt(&self.0, f) }
}
impl<T> Serialize for Id<T> {
	fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
		self.0.serialize(s)
	}
}
impl<'de, T> Deserialize<'de> for Id<T> {
	fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
		Uuid::deserialize(d).map(Self::from_uuid)
	}
}
