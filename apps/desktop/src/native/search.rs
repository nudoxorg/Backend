//! Query adapter from the owner service to desktop row indices.

use backend_client::ClientError;
use backend_library::QueryLimit;
use std::path::Path;

pub(super) fn search_catalog(
    endpoint: &Path,
    query: &str,
) -> Result<Box<[backend_library::RowId]>, ClientError> {
    let limit = QueryLimit::new(QueryLimit::MAX).unwrap_or_default();
    crate::search_endpoint(endpoint, query, limit.get())
}
