//! The one identifier constructor for the whole system.

use std::{fmt, marker::PhantomData};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Strongly-typed id: named by phantom `T` so ids for different entity kinds
/// are incompatible at the type level.
pub struct Id<T>(Uuid, PhantomData<fn() -> T>);

impl<T> Id<T> {
	/// Wrap a raw UUID as a tagged id — the inverse of [`Id::as_uuid`], used when
	/// hydrating a row read back from storage.
	pub const fn from_uuid(uuid: Uuid) -> Self { Self(uuid, PhantomData) }

	/// The raw UUID, for storage keys and wire encoding.
	pub const fn as_uuid(&self) -> &Uuid { &self.0 }

	/// Mint a fresh random (v4) id. Use only for values with no natural,
	/// reproducible key; anything content-derived should use [`Id::from_name`].
	pub fn new_random() -> Self { Self(Uuid::new_v4(), PhantomData) }

	/// Derive a deterministic (v5) id from a namespace and name bytes — the single
	/// primitive every `*Id` derivation (package coordinates, entry URIs, source
	/// names) is built on, so "the same thing" always hashes to the same id.
	pub fn from_name(namespace: &Uuid, name: &[u8]) -> Self {
		Self(Uuid::new_v5(namespace, name), PhantomData)
	}

	/// Re-tag this id as identifying a different type, preserving the UUID.
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
	fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> { self.0.serialize(s) }
}
impl<'de, T> Deserialize<'de> for Id<T> {
	fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
		Uuid::deserialize(d).map(Self::from_uuid)
	}
}
