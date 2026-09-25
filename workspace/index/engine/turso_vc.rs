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

use crate::record::PackageRecord;

/// One package fact stored as a versioned row.
///
/// Primary key is `(ecosystem, name, version)`. `body` is the
/// [`PackageRecord`] JSON. `payload_hash` is the BLAKE3 of that body. An
/// unchanged hash does not create a new revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageFact {
    /// Ecosystem token (`rust`, `typescript`, …).
    pub ecosystem: String,
    /// Canonical package name.
    pub name: String,
    /// Version string as published.
    pub version: String,
    /// BLAKE3 hex of [`Self::body`].
    pub payload_hash: String,
    /// Canonical JSON of the [`PackageRecord`].
    pub body: String,
}

impl VersionedRow for PackageFact {
    const COLUMNS: &'static [&'static str] =
        &["ecosystem", "name", "version", "payload_hash", "body"];
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
            body: text(4, "body")?,
        })
    }

    fn into_row(&self) -> VcRow {
        VcRow::new(vec![
            VcValue::Text(self.ecosystem.clone()),
            VcValue::Text(self.name.clone()),
            VcValue::Text(self.version.clone()),
            VcValue::Text(self.payload_hash.clone()),
            VcValue::Text(self.body.clone()),
        ])
    }
}

impl PackageFact {
    /// Encode `record` as the row body. The hash is BLAKE3 of that JSON.
    pub fn from_record(record: &PackageRecord) -> Result<Self, OrmError> {
        let body = serde_json::to_string(record)
            .map_err(|err| OrmError::Decode(format!("package record json: {err}")))?;
        let payload_hash = blake3::hash(body.as_bytes()).to_hex().to_string();
        Ok(Self {
            ecosystem: record.ecosystem.as_token().to_owned(),
            name: record.canonical_name.to_string(),
            version: record.version.to_string(),
            payload_hash,
            body,
        })
    }

    /// Decode the stored [`PackageRecord`].
    pub fn to_record(&self) -> Result<PackageRecord, OrmError> {
        serde_json::from_str(&self.body)
            .map_err(|err| OrmError::Decode(format!("package record json: {err}")))
    }
}

/// Result of writing a package record into the versioned catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactWrite {
    /// The tip already stores this payload hash. No new revision.
    Unchanged,
    /// A new row revision was committed.
    Revised(CommitId),
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

    /// Version `record` when its payload hash differs from the tip.
    pub fn put_record(&mut self, record: &PackageRecord) -> OrmResult<FactWrite> {
        let fact = PackageFact::from_record(record)?;
        if self
            .get(&fact.ecosystem, &fact.name, &fact.version)
            .is_some_and(|tip| tip.payload_hash == fact.payload_hash)
        {
            return Ok(FactWrite::Unchanged);
        }
        Ok(FactWrite::Revised(self.upsert(&fact)?))
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
            body: hash.into(),
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

    #[test]
    fn an_unchanged_record_does_not_grow_history() {
        use crate::record::{DepEdge, PackageRecord};
        use heart::Language;

        let record = PackageRecord::from_parts(
            Language::Rust,
            "memchr",
            "2.8.3",
            None,
            None,
            Vec::new(),
            None,
            None,
            false,
            vec![DepEdge::runtime("libc")],
        );
        let mut catalog = VersionedCatalog::open().expect("open");
        let FactWrite::Revised(first) = catalog.put_record(&record).expect("put") else {
            panic!("first write revises");
        };
        assert_eq!(
            catalog.put_record(&record).expect("replay"),
            FactWrite::Unchanged
        );
        let padded = PackageRecord::from_parts(
            Language::Rust,
            "memchr",
            "2.8.3",
            None,
            None,
            Vec::new(),
            None,
            None,
            false,
            crate::record::runtime_edges_from_names(&["  libc  ", "libc", " "]),
        );
        assert_eq!(
            catalog.put_record(&padded).expect("normalized replay"),
            FactWrite::Unchanged
        );
        let mut changed = record.clone();
        changed.edges = vec![DepEdge::runtime("windows-sys")];
        let FactWrite::Revised(second) = catalog.put_record(&changed).expect("change") else {
            panic!("a new edge revises");
        };
        assert_ne!(first, second);
        let prior = catalog
            .get_at("rust", "memchr", "2.8.3", first)
            .expect("at")
            .expect("row")
            .to_record()
            .expect("record");
        assert_eq!(prior.edges[0].name.as_str(), "libc");
        let tip = catalog
            .get("rust", "memchr", "2.8.3")
            .expect("tip")
            .to_record()
            .expect("record");
        assert_eq!(tip.edges[0].name.as_str(), "windows-sys");
    }
}
