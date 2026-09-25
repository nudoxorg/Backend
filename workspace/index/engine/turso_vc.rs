//! Row-versioned catalog ledger on the philocalyst Turso fork.
//!
//! Every upsert is one committed row revision. DoltLite stays the SQL facade.

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

/// One package fact. Primary key is the opaque version PID.
///
/// `ecosystem`, `name`, and `version` are the coordinate that seeded the PID.
/// `body` is the identity [`PackageRecord`] JSON. `payload_hash` is its BLAKE3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageFact {
    pub version_pid: String,
    pub ecosystem: String,
    pub name: String,
    pub version: String,
    pub payload_hash: String,
    pub body: String,
}

impl VersionedRow for PackageFact {
    const COLUMNS: &'static [&'static str] = &[
        "version_pid",
        "ecosystem",
        "name",
        "version",
        "payload_hash",
        "body",
    ];
    const PK: &'static [&'static str] = &["version_pid"];
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
            version_pid: text(0, "version_pid")?,
            ecosystem: text(1, "ecosystem")?,
            name: text(2, "name")?,
            version: text(3, "version")?,
            payload_hash: text(4, "payload_hash")?,
            body: text(5, "body")?,
        })
    }

    fn into_row(&self) -> VcRow {
        VcRow::new(vec![
            VcValue::Text(self.version_pid.clone()),
            VcValue::Text(self.ecosystem.clone()),
            VcValue::Text(self.name.clone()),
            VcValue::Text(self.version.clone()),
            VcValue::Text(self.payload_hash.clone()),
            VcValue::Text(self.body.clone()),
        ])
    }
}

impl PackageFact {
    pub fn from_record(record: &PackageRecord) -> Result<Self, OrmError> {
        let mut identity = record.clone();
        identity.edges.clear();
        let body = serde_json::to_string(&identity)
            .map_err(|err| OrmError::Decode(format!("package record json: {err}")))?;
        let payload_hash = blake3::hash(body.as_bytes()).to_hex().to_string();
        Ok(Self {
            version_pid: edge_fact::version_pid_of(
                record.ecosystem.as_token(),
                record.canonical_name.as_str(),
                record.version.as_str(),
            ),
            ecosystem: record.ecosystem.as_token().to_owned(),
            name: record.canonical_name.to_string(),
            version: record.version.to_string(),
            payload_hash,
            body,
        })
    }

    pub fn to_record(&self) -> Result<PackageRecord, OrmError> {
        serde_json::from_str(&self.body)
            .map_err(|err| OrmError::Decode(format!("package record json: {err}")))
    }
}

/// Result of writing a package record into the versioned catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactWrite {
    Unchanged,
    Revised(CommitId),
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct PackageKey {
    pub(super) ecosystem: SmolStr,
    pub(super) package: SmolStr,
    pub(super) version: SmolStr,
}

#[derive(Clone)]
pub(super) struct EdgeTip {
    pub(super) name: SmolStr,
    pub(super) class: SmolStr,
    pub(super) hash: SmolStr,
}

/// In-memory versioned catalog on branch `main`.
pub struct VersionedCatalog {
    db: VersionedDb,
    /// Coordinate to version PID. Kept after a drop so an as-of read still
    /// names the row.
    pub(super) coords: BTreeMap<PackageKey, SmolStr>,
    pub(super) edge_tips: BTreeMap<SmolStr, Vec<EdgeTip>>,
}

impl VersionedCatalog {
    pub fn open() -> OrmResult<Self> {
        let mut db = VersionedDb::new("main")?;
        db.store_mut().config_set("user.name", "index");
        db.store_mut().config_set("user.email", "index@nudox.local");
        Ok(Self {
            db,
            coords: BTreeMap::new(),
            edge_tips: BTreeMap::new(),
        })
    }

    pub fn upsert(&mut self, fact: &PackageFact) -> OrmResult<CommitId> {
        self.coords
            .insert(coordinate(fact), SmolStr::new(&fact.version_pid));
        self.db.table().version(fact, "upsert package fact")
    }

    pub fn put_record(&mut self, record: &PackageRecord) -> OrmResult<FactWrite> {
        let record = self.retain_known_content(record)?;
        let edges = self.sync_edges(&record)?;
        let fact = PackageFact::from_record(&record)?;
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

    pub fn apply_effect(
        &mut self,
        effect: &crate::edge_project::LedgerEffect,
    ) -> OrmResult<FactWrite> {
        match effect {
            crate::edge_project::LedgerEffect::Upsert(record) => self.put_record(record),
            crate::edge_project::LedgerEffect::Remove {
                ecosystem,
                name,
                version,
            } => self.drop_version(ecosystem.as_token(), name, version),
        }
    }

    pub fn drop_version(
        &mut self,
        ecosystem: &str,
        name: &str,
        version: &str,
    ) -> OrmResult<FactWrite> {
        let Some(pid) = self.pid_owned(ecosystem, name, version) else {
            return Ok(FactWrite::Unchanged);
        };
        let tips = self.edge_tips.remove(pid.as_str()).unwrap_or_default();
        let mut commit = None;
        for tip in &tips {
            commit = Some(self.db.table::<EdgeFact>().version_delete(
                &edge_fact::edge_pk(&pid, tip.name.as_str(), tip.class.as_str()),
                "drop edge",
            )?);
        }
        if self.db.table::<PackageFact>().get(&pid_pk(&pid)).is_some() {
            commit = Some(
                self.db
                    .table::<PackageFact>()
                    .version_delete(&pid_pk(&pid), "drop package")?,
            );
        }
        match commit {
            Some(commit) => Ok(FactWrite::Revised(commit)),
            None => Ok(FactWrite::Unchanged),
        }
    }

    pub fn get(&mut self, ecosystem: &str, name: &str, version: &str) -> Option<PackageFact> {
        let pid = self.pid_owned(ecosystem, name, version)?;
        self.db.table().get(&pid_pk(&pid))
    }

    pub fn sync_edges(&mut self, record: &PackageRecord) -> OrmResult<EdgeSync> {
        let pid = SmolStr::new(edge_fact::version_pid_of(
            record.ecosystem.as_token(),
            record.canonical_name.as_str(),
            record.version.as_str(),
        ));
        let current = self.edge_tips.get(&pid).cloned().unwrap_or_default();
        let mut revised = 0;
        let mut unchanged = 0;
        let mut commit = None;
        let mut next = Vec::with_capacity(record.edges.len());
        let mut seen = std::collections::BTreeSet::new();
        for edge in &record.edges {
            let class = edge_fact::class_token_of(edge.class);
            if !seen.insert((edge.name.clone(), class)) {
                continue;
            }
            let hash = edge_fact::hash_edge(edge);
            let same = current.iter().any(|tip| {
                tip.name == edge.name && tip.class.as_str() == class && tip.hash.as_str() == hash
            });
            if same {
                unchanged += 1;
            } else {
                let row = EdgeFact::from_edge(record, edge);
                commit = Some(self.db.table::<EdgeFact>().version(&row, "sync edge")?);
                revised += 1;
            }
            next.push(EdgeTip {
                name: edge.name.clone(),
                class: SmolStr::new(class),
                hash: SmolStr::new(hash),
            });
        }
        let mut removed = 0;
        for stored in &current {
            if next
                .iter()
                .any(|edge| edge.name == stored.name && edge.class == stored.class)
            {
                continue;
            }
            commit = Some(self.db.table::<EdgeFact>().version_delete(
                &edge_fact::edge_pk(pid.as_str(), stored.name.as_str(), stored.class.as_str()),
                "remove edge",
            )?);
            removed += 1;
        }
        if next.is_empty() {
            self.edge_tips.remove(&pid);
        } else {
            self.edge_tips.insert(pid, next);
        }
        Ok(EdgeSync {
            revised,
            unchanged,
            removed,
            commit,
        })
    }

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
        let Some(pid) = self.pid_owned(ecosystem, name, version) else {
            return Ok(None);
        };
        record.edges = self
            .db
            .table::<EdgeFact>()
            .iter_at(at)
            .filter(|edge| edge.version_pid == pid.as_str())
            .map(|edge| edge.to_edge())
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(record))
    }

    fn tip_edges(&mut self, ecosystem: &str, name: &str, version: &str) -> OrmResult<Vec<DepEdge>> {
        let Some(pid) = self.pid_owned(ecosystem, name, version) else {
            return Ok(Vec::new());
        };
        let Some(tips) = self.edge_tips.get(pid.as_str()).cloned() else {
            return Ok(Vec::new());
        };
        let mut edges = Vec::with_capacity(tips.len());
        for tip in tips {
            let Some(row) = self.db.table::<EdgeFact>().get(&edge_fact::edge_pk(
                &pid,
                tip.name.as_str(),
                tip.class.as_str(),
            )) else {
                continue;
            };
            edges.push(row.to_edge()?);
        }
        Ok(edges)
    }

    pub fn scan_edges(&mut self) -> Vec<EdgeFact> {
        self.db.table::<EdgeFact>().iter().collect()
    }

    pub fn get_edge(
        &mut self,
        ecosystem: &str,
        package: &str,
        version: &str,
        name: &str,
    ) -> Option<EdgeFact> {
        let pid = self.pid_owned(ecosystem, package, version)?;
        edge_fact::class_tokens().iter().find_map(|class| {
            self.db
                .table::<EdgeFact>()
                .get(&edge_fact::edge_pk(&pid, name, class))
        })
    }

    fn pid_owned(&self, ecosystem: &str, name: &str, version: &str) -> Option<SmolStr> {
        self.coords
            .get(&PackageKey {
                ecosystem: SmolStr::new(ecosystem),
                package: SmolStr::new(name),
                version: SmolStr::new(version),
            })
            .cloned()
    }

    pub fn get_at(
        &mut self,
        ecosystem: &str,
        name: &str,
        version: &str,
        at: CommitId,
    ) -> OrmResult<Option<PackageFact>> {
        let Some(pid) = self.pid_owned(ecosystem, name, version) else {
            return Ok(None);
        };
        self.db.table().try_get_at(&pid_pk(&pid), at)
    }
}

fn coordinate(fact: &PackageFact) -> PackageKey {
    PackageKey {
        ecosystem: SmolStr::new(&fact.ecosystem),
        package: SmolStr::new(&fact.name),
        version: SmolStr::new(&fact.version),
    }
}

fn pid_pk(version_pid: &str) -> Vec<VcValue> {
    vec![VcValue::Text(version_pid.to_owned())]
}

#[cfg(test)]
#[path = "turso_vc_tests.rs"]
mod tests;
