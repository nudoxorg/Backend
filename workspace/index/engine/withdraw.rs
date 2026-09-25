//! A withdrawn version stays addressable. Its edges stay on the tip.

use turso_versioning::orm::OrmResult;

use super::turso_vc::{FactWrite, VersionedCatalog};

impl VersionedCatalog {
    /// Set `yanked` on the tip row. Missing rows are left absent.
    ///
    /// Edges already stored on the tip are written back with the flag, so a
    /// withdrawal does not look like an empty dependency list.
    pub fn withdraw(&mut self, ecosystem: &str, name: &str, version: &str) -> OrmResult<FactWrite> {
        let Some(mut record) = self.materialize(ecosystem, name, version)? else {
            return Ok(FactWrite::Unchanged);
        };
        record.yanked = true;
        self.put_record(&record)
    }
}
