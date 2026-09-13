//! Bounded registry acquisition and durable sparse-index ingestion.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod checksum;
mod crates_io;
mod pipeline;
#[cfg(test)]
mod tests;
mod transport;

pub use checksum::{ArchiveChecksum, RegistryError};
pub use crates_io::{CratesIoAdapter, SPARSE_CHUNK_ROWS};
pub use pipeline::{
    IngestError, IngestOutcome, MaterializedPublication, RegistryIngestor, RegistryMaterializer,
    VerifiedArchive,
};
pub use transport::{
    MAX_ARCHIVE_BYTES, MAX_PAGE_BYTES, RegistryTransport, TransportFault, TransportRequest,
    TransportResponse, UreqTransport,
};
