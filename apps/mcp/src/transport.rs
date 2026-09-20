//! Shared command transport exports for the MCP surface.

#[cfg(unix)]
pub use backend_client::UnixCommandTransport;
pub use backend_client::{
    CertifiedCommandTransport, CommandTransport, InProcessTransport, LocalEngine,
};

#[cfg(test)]
pub(crate) fn write_frame(writer: &mut impl std::io::Write, body: &[u8]) -> std::io::Result<()> {
    backend_replication::write_frame(writer, body, crate::protocol::control_limits())
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()))
}

#[cfg(test)]
pub(crate) fn read_frame(reader: &mut impl std::io::Read) -> std::io::Result<Vec<u8>> {
    backend_replication::read_frame(reader, crate::protocol::control_limits())
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()))
}
