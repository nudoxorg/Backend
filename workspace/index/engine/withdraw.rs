//! A withdrawn version stays addressable. Its edges stay on the tip.
//!
//! Resolution goes through [`crate::pid::PidKernel`]: the version PID does
//! not change when the row is yanked.

use turso_versioning::orm::OrmResult;

use crate::pid::{BoundPid, PidKernel, Resolve, VersionPid};

use super::turso_vc::{FactWrite, VersionedCatalog};

impl VersionedCatalog {
    /// Resolve one published coordinate. A yanked tip is a tombstone of the
    /// same version PID. An unknown coordinate is absent.
    pub fn resolve_version(
        &mut self,
        ecosystem: &str,
        name: &str,
        version: &str,
    ) -> OrmResult<Option<Resolve>> {
        let Some(fact) = self.get(ecosystem, name, version) else {
            return Ok(None);
        };
        let record = fact.to_record()?;
        let kernel = PidKernel {
            pid: BoundPid::Version(VersionPid::mint(ecosystem, name, version)),
            locations: record.repository.into_iter().collect(),
            withdrawn: record.yanked,
            checksum: None,
        };
        debug_assert_eq!(
            fact.version_pid,
            VersionPid::mint(ecosystem, name, version).local_name()
        );
        Ok(Some(kernel.resolve()))
    }

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
