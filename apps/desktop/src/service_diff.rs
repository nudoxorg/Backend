//! Shared daemon-backed semantic diff seam for the native desktop surface.

use backend_client::{ClientError, Session};
use backend_library::{DiffRecord, PackageReference};
use std::path::Path;

/// Compares two indexed package versions through the compiler publication
/// owner shared by CLI and MCP.
///
/// Package syntax is admitted before connecting, then the daemon reopens the
/// exact semantic image claims and returns its bounded typed result.
///
/// # Errors
///
/// Returns a protocol error for an invalid package reference or the typed
/// client failure from transport, command, or reply admission.
pub fn diff_endpoint(
    endpoint: &Path,
    from: &str,
    to: &str,
) -> Result<Box<[DiffRecord]>, ClientError> {
    let from = package_reference("older", from)?;
    let to = package_reference("newer", to)?;
    let mut session = Session::connect(endpoint)?;
    session.diff(from, to)
}

fn package_reference(position: &str, value: &str) -> Result<PackageReference, ClientError> {
    PackageReference::parse(value.to_owned())
        .map_err(|error| ClientError::Protocol(format!("invalid {position} package: {error}")))
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn invalid_package_is_rejected_before_endpoint_io() {
        let error = diff_endpoint(
            Path::new("/an/endpoint/that/does/not/exist"),
            "pkg:cargo/example",
            "pkg:cargo/example@2.0.0",
        )
        .expect_err("unpinned package");
        assert!(matches!(error, ClientError::Protocol(message) if message.contains("older")));
    }
}
