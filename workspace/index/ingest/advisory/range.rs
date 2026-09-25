use super::{window::VersionWindow, *};

/// Whether `version` falls inside an OSV event range.
///
/// `introduced:0` opens the window at the beginning. `fixed` is exclusive.
/// `last_affected` is inclusive. A version that is not semver (including a
/// git revision) is outside the window. An explicit comma-separated version
/// list is not an event range.
pub fn version_in_osv_range(range: &str, version: &str) -> bool {
    VersionWindow::parse(range).is_some_and(|window| window.contains(version))
}

pub(super) fn is_event_range(range: &str) -> bool {
    VersionWindow::parse(range).is_some_and(|window| window.is_events())
}

/// Advisory listings for catalog versions that an event range already covers.
///
/// Explicit version lists stay on [`AdvisorySource::catalog_ops`]. A withdrawn
/// advisory and a range with no resolved stem add nothing here.
pub fn range_listings_for(source: &AdvisorySource, known: &[(PackageId, &str)]) -> Vec<CatalogOp> {
    if source.valid_to.is_some() {
        return Vec::new();
    }
    let Some(range) = source.version_range.as_deref() else {
        return Vec::new();
    };
    if !is_event_range(range) {
        return Vec::new();
    }
    known
        .iter()
        .filter(|(_, version)| version_in_osv_range(range, version))
        .map(|(version, _)| {
            super::advisory_listing_event(*version, source.valid_from, &source.upstream_id)
        })
        .collect()
}
