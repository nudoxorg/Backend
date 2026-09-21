//! Shared command transport exports for the CLI surface.

#[cfg(any(unix, windows))]
pub use backend_client::UnixCommandTransport;
pub use backend_client::{
    CertifiedCommandTransport, CommandTransport, InProcessTransport, LocalEngine,
};

#[cfg(test)]
pub(crate) fn write_frame(
    writer: &mut impl std::io::Write,
    body: &[u8],
) -> Result<(), crate::ClientError> {
    backend_replication::write_frame(writer, body, crate::protocol::control_limits())
        .map_err(crate::protocol::map_frame_error)
}

#[cfg(test)]
pub(crate) fn read_frame(reader: &mut impl std::io::Read) -> Result<Vec<u8>, crate::ClientError> {
    backend_replication::read_frame(reader, crate::protocol::control_limits())
        .map_err(crate::protocol::map_frame_error)
}
