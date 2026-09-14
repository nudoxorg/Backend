//! Bounded registry acquisition and durable sparse-index ingestion.

mod checksum;
mod crates_io;
mod pipeline;
#[cfg(test)]
mod tests;
mod transport;

pub use self::checksum::{ArchiveChecksum, RegistryError};
pub use self::crates_io::{CratesIoAdapter, SPARSE_CHUNK_ROWS};
pub use self::pipeline::{
    IngestError, IngestOutcome, MaterializedPublication, RegistryIngestor, RegistryMaterializer,
    VerifiedArchive,
};
pub use self::transport::{
    MAX_ARCHIVE_BYTES, MAX_PAGE_BYTES, RegistryTransport, TransportFault, TransportRequest,
    TransportResponse, UreqTransport,
};
