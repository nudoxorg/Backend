#![no_std]
#![forbid(unsafe_code)]
//! Canonical sparse immutable object-pack writing.

#[cfg(not(any(target_pointer_width = "32", target_pointer_width = "64")))]
compile_error!("nudox-object-pack requires 32-bit or 64-bit usize coordinates");

mod error;
mod format;
mod header;
mod index;
mod write;

pub use error::ObjectPackError;
pub use format::{ObjectPackBytes, ObjectPackObjectCount};
pub use header::{OBJECT_PACK_HEADER_BYTES, ObjectPackHeader, ObjectPackHeaderFacts};
pub use index::{ObjectPackIndex, ObjectPackIndexFacts};
pub use write::{ObjectPackFacts, PackInput, PreparedObjectPack};
