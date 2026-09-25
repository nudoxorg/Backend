//! One dependency edge as its own versioned row.

use turso_versioning::{
    orm::{OrmError, VersionedRow},
    vtab_log::{VcRow, VcValue},
};

use crate::record::{DepClass, DepEdge, PackageRecord};

/// One dependency edge, versioned apart from the package body.
///
/// A requirement change revises this row only. The package fact stays on its
/// previous commit until its own payload changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeFact {
    /// Opaque [`crate::pid::VersionPid`] of the depending package version.
    pub version_pid: String,
    /// Ecosystem token of the depending package.
    pub ecosystem: String,
    /// Canonical name of the depending package.
    pub package: String,
    /// Version of the depending package.
    pub version: String,
    /// Dependency name.
    pub name: String,
    /// Dependency ecosystem token. Empty when the edge is in the depending
    /// package's own ecosystem.
    pub dep_ecosystem: String,
    /// Requirement expression. Empty when the manifest named none.
    pub requirement: String,
    /// [`DepClass`] token.
    pub class: String,
    /// `1` when the manifest marked the edge optional.
    pub optional: String,
    /// BLAKE3 hex of requirement, class, and optional.
    pub payload_hash: String,
}

impl VersionedRow for EdgeFact {
    const COLUMNS: &'static [&'static str] = &[
        "version_pid",
        "ecosystem",
        "package",
        "version",
        "name",
        "dep_ecosystem",
        "requirement",
        "class",
        "optional",
        "payload_hash",
    ];
    const PK: &'static [&'static str] = &["version_pid", "dep_ecosystem", "name", "class"];
    const TABLE: &'static str = "package_edges";

    fn from_row(row: &VcRow) -> Result<Self, OrmError> {
        let text = |idx: usize, field: &str| -> Result<String, OrmError> {
            row.values
                .get(idx)
                .and_then(VcValue::as_text)
                .map(str::to_owned)
                .ok_or_else(|| OrmError::Decode(format!("package_edges.{field}")))
        };
        Ok(Self {
            version_pid: text(0, "version_pid")?,
            ecosystem: text(1, "ecosystem")?,
            package: text(2, "package")?,
            version: text(3, "version")?,
            name: text(4, "name")?,
            dep_ecosystem: text(5, "dep_ecosystem")?,
            requirement: text(6, "requirement")?,
            class: text(7, "class")?,
            optional: text(8, "optional")?,
            payload_hash: text(9, "payload_hash")?,
        })
    }

    fn into_row(&self) -> VcRow {
        VcRow::new(vec![
            VcValue::Text(self.version_pid.clone()),
            VcValue::Text(self.ecosystem.clone()),
            VcValue::Text(self.package.clone()),
            VcValue::Text(self.version.clone()),
            VcValue::Text(self.name.clone()),
            VcValue::Text(self.dep_ecosystem.clone()),
            VcValue::Text(self.requirement.clone()),
            VcValue::Text(self.class.clone()),
            VcValue::Text(self.optional.clone()),
            VcValue::Text(self.payload_hash.clone()),
        ])
    }
}

impl EdgeFact {
    pub(in crate::engine) fn from_edge(record: &PackageRecord, edge: &DepEdge) -> Self {
        Self {
            version_pid: version_pid_of(
                record.ecosystem.as_token(),
                record.canonical_name.as_str(),
                record.version.as_str(),
            ),
            ecosystem: record.ecosystem.as_token().to_owned(),
            package: record.canonical_name.to_string(),
            version: record.version.to_string(),
            name: edge.name.to_string(),
            dep_ecosystem: edge
                .dep_ecosystem
                .map(|ecosystem| ecosystem.as_token().to_owned())
                .unwrap_or_default(),
            requirement: edge.requirement.as_deref().unwrap_or("").to_owned(),
            class: class_token(edge.class).to_owned(),
            optional: if edge.optional { "1" } else { "0" }.to_owned(),
            payload_hash: hash_edge(edge),
        }
    }
}

/// How many edge rows a sync revised, left alone, or removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EdgeSync {
    /// Rows whose payload hash differed from the tip.
    pub revised: usize,
    /// Rows already stored at the same hash.
    pub unchanged: usize,
    /// Tip rows absent from the new edge list.
    pub removed: usize,
    /// Last commit this sync produced. `None` when every edge was already
    /// current.
    pub commit: Option<turso_versioning::model::CommitId>,
}

/// Opaque version PID for a published coordinate. The coordinate is the seed,
/// not the stored key.
pub(in crate::engine) fn version_pid_of(ecosystem: &str, name: &str, version: &str) -> String {
    crate::pid::VersionPid::mint(ecosystem, name, version).local_name()
}

pub(in crate::engine) fn edge_pk(
    version_pid: &str,
    dep_ecosystem: &str,
    name: &str,
    class: &str,
) -> Vec<VcValue> {
    vec![
        VcValue::Text(version_pid.to_owned()),
        VcValue::Text(dep_ecosystem.to_owned()),
        VcValue::Text(name.to_owned()),
        VcValue::Text(class.to_owned()),
    ]
}

pub(in crate::engine) fn class_tokens() -> &'static [&'static str] {
    &["runtime", "dev", "build", "optional", "peer"]
}

impl EdgeFact {
    /// Rebuild the manifest edge this row stores.
    pub(in crate::engine) fn to_edge(&self) -> Result<DepEdge, OrmError> {
        Ok(DepEdge {
            name: smol_str::SmolStr::new(&self.name),
            dep_ecosystem: if self.dep_ecosystem.is_empty() {
                None
            } else {
                Some(
                    heart::Language::from_token(&self.dep_ecosystem)
                        .ok_or_else(|| OrmError::Decode(format!("package_edges.dep_ecosystem {}", self.dep_ecosystem)))?,
                )
            },
            requirement: if self.requirement.is_empty() {
                None
            } else {
                Some(smol_str::SmolStr::new(&self.requirement))
            },
            class: class_from_token(&self.class)?,
            optional: self.optional == "1",
        })
    }
}

pub(in crate::engine) fn class_token_of(class: DepClass) -> &'static str {
    class_token(class)
}

fn class_token(class: DepClass) -> &'static str {
    match class {
        DepClass::Runtime => "runtime",
        DepClass::Dev => "dev",
        DepClass::Build => "build",
        DepClass::Optional => "optional",
        DepClass::Peer => "peer",
    }
}

fn class_from_token(token: &str) -> Result<DepClass, OrmError> {
    match token {
        "runtime" => Ok(DepClass::Runtime),
        "dev" => Ok(DepClass::Dev),
        "build" => Ok(DepClass::Build),
        "optional" => Ok(DepClass::Optional),
        "peer" => Ok(DepClass::Peer),
        other => Err(OrmError::Decode(format!("package_edges.class {other}"))),
    }
}

/// BLAKE3 of requirement, class, optional, and the dependency ecosystem.
/// Borrows the manifest edge, so an unchanged tip can reject the edge before
/// a row is allocated. An empty ecosystem token means the depender's own.
pub(in crate::engine) fn hash_edge(edge: &DepEdge) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(edge.requirement.as_deref().unwrap_or("").as_bytes());
    hasher.update(&[0xff]);
    hasher.update(class_token(edge.class).as_bytes());
    hasher.update(&[0xff]);
    hasher.update(if edge.optional { b"1".as_slice() } else { b"0".as_slice() });
    hasher.update(&[0xff]);
    hasher.update(
        edge.dep_ecosystem
            .map(|ecosystem| ecosystem.as_token())
            .unwrap_or("")
            .as_bytes(),
    );
    hasher.finalize().to_hex().to_string()
}
