//! The canonical, version-agnostic GUID a package/symbol is known by — derived
//! deterministically (see [`crate::identity`]) and persisted in postgres so the
//! whole system agrees on one identity.
//!
//! IMPLEMENT HERE: minting + looking up canonical GUIDs against postgres.
