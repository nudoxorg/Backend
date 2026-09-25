//! A later upsert that did not observe a digest keeps the one already stored.

use turso_versioning::orm::OrmResult;

use crate::record::PackageRecord;

use super::turso_vc::VersionedCatalog;

impl VersionedCatalog {
    pub(super) fn retain_known_content(
        &mut self,
        record: &PackageRecord,
    ) -> OrmResult<PackageRecord> {
        if record.content.is_some() {
            return Ok(record.clone());
        }
        let Some(prior) = self.get(
            record.ecosystem.as_token(),
            record.canonical_name.as_str(),
            record.version.as_str(),
        ) else {
            return Ok(record.clone());
        };
        let mut kept = record.clone();
        kept.content = prior.to_record()?.content;
        Ok(kept)
    }
}
