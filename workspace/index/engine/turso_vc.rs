//! Row-versioned catalog ledger on the philocalyst Turso fork.
//!
//! [`turso_versioning::orm::VersionedDb`] is the storage engine: every upsert
//! is one committed row revision (`TableHandle::version`), readable later with
//! `try_get_at`. This is the index's single versioned-row store. DoltLite
//! remains the SQL facade for the existing catalog tables; new frontier writes
//! go through [`VersionedCatalog`].

use turso_versioning::{
    model::CommitId,
    orm::{OrmError, OrmResult, VersionedDb, VersionedRow},
    vtab_log::{VcRow, VcValue},
};

use std::collections::BTreeMap;

use smol_str::SmolStr;

use super::edge_fact;
use crate::record::{DepEdge, PackageRecord};

pub use edge_fact::{EdgeFact, EdgeSync};

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
    /// Encode `record` as the row body. Edges are stored on `package_edges`,
    /// so the hash covers identity and metadata only.
    pub fn from_record(record: &PackageRecord) -> Result<Self, OrmError> {
        let mut identity = record.clone();
        identity.edges.clear();
        let body = serde_json::to_string(&identity)
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

/// Dependency names and hashes for one package version, in first-seen order.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PackageKey {
    ecosystem: SmolStr,
    package: SmolStr,
    version: SmolStr,
}

#[derive(Clone)]
struct EdgeTip {
    name: SmolStr,
    hash: SmolStr,
}

/// In-memory versioned catalog on branch `main`.
pub struct VersionedCatalog {
    db: VersionedDb,
    /// Tip hashes for one package version. A requirement change looks here
    /// instead of walking every edge row in the catalog.
    edge_tips: BTreeMap<PackageKey, Vec<EdgeTip>>,
}

impl VersionedCatalog {
    /// Open an empty catalog on branch `main`.
    pub fn open() -> OrmResult<Self> {
        let mut db = VersionedDb::new("main")?;
        db.store_mut().config_set("user.name", "index");
        db.store_mut().config_set("user.email", "index@nudox.local");
        Ok(Self {
            db,
            edge_tips: BTreeMap::new(),
        })
    }

    /// Insert or replace one fact and commit that row. Returns the new
    /// revision.
    pub fn upsert(&mut self, fact: &PackageFact) -> OrmResult<CommitId> {
        self.db.table().version(fact, "upsert package fact")
    }

    /// Version identity and each edge. An edge-only change leaves the package
    /// row on its previous commit and still reports [`FactWrite::Revised`].
    pub fn put_record(&mut self, record: &PackageRecord) -> OrmResult<FactWrite> {
        let edges = self.sync_edges(record)?;
        let fact = PackageFact::from_record(record)?;
        let same = self
            .get(&fact.ecosystem, &fact.name, &fact.version)
            .is_some_and(|tip| tip.payload_hash == fact.payload_hash);
        if same && edges.revised == 0 && edges.removed == 0 {
            return Ok(FactWrite::Unchanged);
        }
        if same {
            return Ok(FactWrite::Revised(
                edges.commit.expect("an edge write has a commit"),
            ));
        }
        Ok(FactWrite::Revised(self.upsert(&fact)?))
    }

    /// Tip read.
    pub fn get(&mut self, ecosystem: &str, name: &str, version: &str) -> Option<PackageFact> {
        self.db.table().get(&pk(ecosystem, name, version))
    }

    /// Version each edge of `record` on its own row.
    ///
    /// An edge whose requirement, class, and optional flag already match the
    /// tip is left in place. An edge missing from `record` is deleted at the
    /// tip and remains readable at the earlier commit.
    pub fn sync_edges(&mut self, record: &PackageRecord) -> OrmResult<EdgeSync> {
        let key = PackageKey {
            ecosystem: SmolStr::new(record.ecosystem.as_token()),
            package: record.canonical_name.clone(),
            version: record.version.clone(),
        };
        let current = self.edge_tips.get(&key).cloned().unwrap_or_default();
        let mut revised = 0;
        let mut unchanged = 0;
        let mut commit = None;
        let mut next = Vec::with_capacity(record.edges.len());
        let mut seen = std::collections::BTreeSet::new();
        for edge in &record.edges {
            if !seen.insert(edge.name.clone()) {
                continue;
            }
            let hash = edge_fact::hash_edge(edge);
            let same = current
                .iter()
                .any(|tip| tip.name == edge.name && tip.hash.as_str() == hash);
            if same {
                unchanged += 1;
            } else {
                let row = EdgeFact::from_edge(record, edge);
                commit = Some(self.db.table::<EdgeFact>().version(&row, "sync edge")?);
                revised += 1;
            }
            next.push(EdgeTip {
                name: edge.name.clone(),
                hash: SmolStr::new(hash),
            });
        }
        let mut removed = 0;
        for stored in &current {
            if next.iter().any(|edge| edge.name == stored.name) {
                continue;
            }
            commit = Some(self.db.table::<EdgeFact>().version_delete(
                &edge_fact::edge_pk(
                    key.ecosystem.as_str(),
                    key.package.as_str(),
                    key.version.as_str(),
                    stored.name.as_str(),
                ),
                "remove edge",
            )?);
            removed += 1;
        }
        if next.is_empty() {
            self.edge_tips.remove(&key);
        } else {
            self.edge_tips.insert(key, next);
        }
        Ok(EdgeSync {
            revised,
            unchanged,
            removed,
            commit,
        })
    }

    /// Tip record with dependency edges joined back on.
    pub fn materialize(
        &mut self,
        ecosystem: &str,
        name: &str,
        version: &str,
    ) -> OrmResult<Option<PackageRecord>> {
        let Some(fact) = self.get(ecosystem, name, version) else {
            return Ok(None);
        };
        let mut record = fact.to_record()?;
        record.edges = self.tip_edges(ecosystem, name, version)?;
        Ok(Some(record))
    }

    /// Record as of `at`, with the edge rows that existed at that commit.
    pub fn materialize_at(
        &mut self,
        ecosystem: &str,
        name: &str,
        version: &str,
        at: CommitId,
    ) -> OrmResult<Option<PackageRecord>> {
        let Some(fact) = self.get_at(ecosystem, name, version, at)? else {
            return Ok(None);
        };
        let mut record = fact.to_record()?;
        record.edges = self
            .db
            .table::<EdgeFact>()
            .iter_at(at)
            .filter(|edge| {
                edge.ecosystem == ecosystem && edge.package == name && edge.version == version
            })
            .map(|edge| edge.to_edge())
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(record))
    }

    fn tip_edges(&mut self, ecosystem: &str, name: &str, version: &str) -> OrmResult<Vec<DepEdge>> {
        let key = PackageKey {
            ecosystem: SmolStr::new(ecosystem),
            package: SmolStr::new(name),
            version: SmolStr::new(version),
        };
        let Some(tips) = self.edge_tips.get(&key).cloned() else {
            return Ok(Vec::new());
        };
        let mut edges = Vec::with_capacity(tips.len());
        for tip in tips {
            let Some(row) = self.get_edge(ecosystem, name, version, tip.name.as_str()) else {
                continue;
            };
            edges.push(row.to_edge()?);
        }
        Ok(edges)
    }

    /// Every edge row at the tip. The scan the per-package tip replaces.
    pub fn scan_edges(&mut self) -> Vec<EdgeFact> {
        self.db.table::<EdgeFact>().iter().collect()
    }

    /// Tip edge, when it has not been deleted.
    pub fn get_edge(
        &mut self,
        ecosystem: &str,
        package: &str,
        version: &str,
        name: &str,
    ) -> Option<EdgeFact> {
        self.db
            .table::<EdgeFact>()
            .get(&edge_fact::edge_pk(ecosystem, package, version, name))
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
#[path = "turso_vc_tests.rs"]
mod tests;
