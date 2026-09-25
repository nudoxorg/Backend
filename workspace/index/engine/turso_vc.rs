//! Row-versioned catalog ledger on the philocalyst Turso fork.
//!
//! [`turso_versioning::orm::VersionedDb`] is the storage engine: every upsert
//! is one committed row revision (`TableHandle::version`), readable later with
//! `try_get_at`. This is the index's single versioned-row store. DoltLite
//! remains the SQL facade for the existing catalog tables; new frontier writes
//! go through [`VersionedCatalog`].

use turso_versioning::model::CommitId;
use turso_versioning::orm::{OrmError, OrmResult, VersionedDb, VersionedRow};
use turso_versioning::vtab_log::{VcRow, VcValue};

/// One package fact stored as a versioned row.
///
/// Primary key is `(ecosystem, name, version)`. `payload_hash` is the content
/// identity of the fact body; a rewrite with the same hash is still a new
/// revision only when the caller chooses to version it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageFact {
    /// Ecosystem token (`rust`, `npm`, …).
    pub ecosystem: String,
    /// Canonical package name.
    pub name: String,
    /// Version string as published.
    pub version: String,
    /// BLAKE3 hex (or any stable digest) of the fact payload.
    pub payload_hash: String,
}

impl VersionedRow for PackageFact {
    const COLUMNS: &'static [&'static str] = &["ecosystem", "name", "version", "payload_hash"];
    const PK: &'static [&'static str] = &["ecosystem", "name", "version"];
    const TABLE: &'static str = "package_facts";

    fn from_row(row: &VcRow) -> Result<Self, OrmError> {
        let text = |idx: usize, field: &str| -> Result<String, OrmError> {
            row.values
                .get(idx)
                .and_then(VcValue::as_text)
                .map(str::to_owned)
                .ok_or_else(|| OrmError::Decode(format!("package_facts.{field}")))
        };
        Ok(Self {
            ecosystem: text(0, "ecosystem")?,
            name: text(1, "name")?,
            version: text(2, "version")?,
            payload_hash: text(3, "payload_hash")?,
        })
    }

    fn into_row(&self) -> VcRow {
        VcRow::new(vec![
            VcValue::Text(self.ecosystem.clone()),
            VcValue::Text(self.name.clone()),
            VcValue::Text(self.version.clone()),
            VcValue::Text(self.payload_hash.clone()),
        ])
    }
}

/// In-memory versioned catalog on branch `main`.
pub struct VersionedCatalog {
    db: VersionedDb,
}

impl VersionedCatalog {
    /// Open an empty catalog on branch `main`.
    pub fn open() -> OrmResult<Self> {
        let mut db = VersionedDb::new("main")?;
        db.store_mut().config_set("user.name", "index");
        db.store_mut().config_set("user.email", "index@nudox.local");
        Ok(Self { db })
    }

    /// Insert or replace one fact and commit that row. Returns the new revision.
    pub fn upsert(&mut self, fact: &PackageFact) -> OrmResult<CommitId> {
        self.db.table().version(fact, "upsert package fact")
    }

    /// Tip read.
    pub fn get(&mut self, ecosystem: &str, name: &str, version: &str) -> Option<PackageFact> {
        self.db.table().get(&pk(ecosystem, name, version))
    }

    /// Historical read at `at`. `None` when the row did not exist then.
    pub fn get_at(
        &mut self,
        ecosystem: &str,
        name: &str,
        version: &str,
        at: CommitId,
    ) -> OrmResult<Option<PackageFact>> {
        self.db
            .table()
            .try_get_at(&pk(ecosystem, name, version), at)
    }
}

fn pk(ecosystem: &str, name: &str, version: &str) -> Vec<VcValue> {
    vec![
        VcValue::Text(ecosystem.to_owned()),
        VcValue::Text(name.to_owned()),
        VcValue::Text(version.to_owned()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(hash: &str) -> PackageFact {
        PackageFact {
            ecosystem: "rust".into(),
            name: "memchr".into(),
            version: "2.8.3".into(),
            payload_hash: hash.into(),
        }
    }

    #[test]
    fn upsert_is_a_new_revision_and_history_reads_back() {
        let mut catalog = VersionedCatalog::open().expect("open");
        let first = catalog.upsert(&fact("aaa")).expect("first");
        let second = catalog.upsert(&fact("bbb")).expect("second");
        assert_ne!(first, second);
        let tip = catalog.get("rust", "memchr", "2.8.3").expect("tip");
        assert_eq!(tip.payload_hash, "bbb");
        let prior = catalog
            .get_at("rust", "memchr", "2.8.3", first)
            .expect("at")
            .expect("row existed");
        assert_eq!(prior.payload_hash, "aaa");
    }
}
