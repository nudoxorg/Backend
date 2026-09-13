//! The `backend_store::object_pack` module writes and borrows indexed immutable object packs.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
//! Canonical sparse immutable object-pack writing.

#[cfg(not(any(target_pointer_width = "32", target_pointer_width = "64")))]
compile_error!("object packs require 32-bit or 64-bit usize coordinates");

mod error;
mod format;
mod header;
mod index;
mod view;
mod write;

pub use self::error::ObjectPackError;
pub use self::format::{ObjectPackBytes, ObjectPackObjectCount};
pub use self::header::{OBJECT_PACK_HEADER_BYTES, ObjectPackHeader, ObjectPackHeaderFacts};
pub use self::index::{ObjectPackIndex, ObjectPackIndexFacts};
pub use self::view::{ObjectPackObject, ObjectPackView};
pub use self::write::{ObjectPackFacts, PackInput, PreparedObjectPack};
