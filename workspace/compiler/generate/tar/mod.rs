//! A streaming tar *view* of a package's source.
//!
//! The stored representation is content-addressed per file (see
//! [`super::source_archive`]); this module produces a tar on demand (e.g. to
//! hand a caller the whole source at once) by streaming entries into a writer
//! without ever buffering the full archive in memory.

use std::{io::Write, path::PathBuf};

use bytes::Bytes;

use crate::error::GenerateError;

/// One entry to write into the tar: its path and its bytes (typically fetched
/// lazily from the content-addressed store, so the whole archive is never
/// resident at once).
pub struct TarEntry {
	/// Path within the archive.
	pub path: PathBuf,
	/// The file's bytes.
	pub bytes: Bytes,
}

/// Stream `entries` into `writer` as a tar archive, flushing as it goes so peak
/// memory is one entry, not the whole archive.
pub fn write_tar<W: Write>(
	writer: W,
	entries: impl Iterator<Item = Result<TarEntry, GenerateError>>,
) -> Result<(), GenerateError> {
	let _ = (writer, entries);
	todo!("tar::Builder::new(writer); append each entry; finish()")
}
