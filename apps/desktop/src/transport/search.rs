//! Shared daemon-backed search seam for the native desktop surface.

use backend_client::{ClientError, Session};
use backend_library::{CommandReply, RowId};
use std::path::Path;

/// Searches through the same owner session used by CLI and MCP.
///
/// The daemon selects rows through its admitted Tantivy lane and returns a
/// proof-bearing snapshot. The desktop retains only stable identities and
/// resolves them against its currently installed immutable catalog.
///
/// # Errors
///
/// Returns the typed client failure when transport, command, or reply
/// admission fails.
pub fn search_endpoint(
    endpoint: &Path,
    query: &str,
    limit: u16,
) -> Result<Box<[RowId]>, ClientError> {
    let mut session = Session::connect(endpoint)?;
    let reply = session.search(query, limit)?;
    let CommandReply::Search(snapshot) = reply.reply else {
        return Err(ClientError::Protocol(
            "desktop search reply changed shape".to_owned(),
        ));
    };
    Ok(snapshot
        .root
        .rows()
        .iter()
        .map(|row| row.id)
        .collect::<Vec<_>>()
        .into_boxed_slice())
}
