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
    /// Ecosystem token of the depending package.
    pub ecosystem: String,
    /// Canonical name of the depending package.
    pub package: String,
    /// Version of the depending package.
    pub version: String,
    /// Dependency name.
    pub name: String,
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
        "ecosystem",
        "package",
        "version",
        "name",
        "requirement",
        "class",
        "optional",
        "payload_hash",
    ];
    const PK: &'static [&'static str] = &["ecosystem", "package", "version", "name"];
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
            ecosystem: text(0, "ecosystem")?,
            package: text(1, "package")?,
            version: text(2, "version")?,
            name: text(3, "name")?,
            requirement: text(4, "requirement")?,
            class: text(5, "class")?,
            optional: text(6, "optional")?,
            payload_hash: text(7, "payload_hash")?,
        })
    }

    fn into_row(&self) -> VcRow {
        VcRow::new(vec![
            VcValue::Text(self.ecosystem.clone()),
            VcValue::Text(self.package.clone()),
            VcValue::Text(self.version.clone()),
            VcValue::Text(self.name.clone()),
            VcValue::Text(self.requirement.clone()),
            VcValue::Text(self.class.clone()),
            VcValue::Text(self.optional.clone()),
            VcValue::Text(self.payload_hash.clone()),
        ])
    }
}

impl EdgeFact {
    pub(in crate::engine) fn from_edge(record: &PackageRecord, edge: &DepEdge) -> Self {
        let requirement = edge.requirement.as_deref().unwrap_or("").to_owned();
        let class = class_token(edge.class).to_owned();
        let optional = if edge.optional { "1" } else { "0" }.to_owned();
        let payload_hash = edge_hash(&requirement, &class, &optional);
        Self {
            ecosystem: record.ecosystem.as_token().to_owned(),
            package: record.canonical_name.to_string(),
            version: record.version.to_string(),
            name: edge.name.to_string(),
            requirement,
            class,
            optional,
            payload_hash,
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

pub(in crate::engine) fn edge_pk(
    ecosystem: &str,
    package: &str,
    version: &str,
    name: &str,
) -> Vec<VcValue> {
    vec![
        VcValue::Text(ecosystem.to_owned()),
        VcValue::Text(package.to_owned()),
        VcValue::Text(version.to_owned()),
        VcValue::Text(name.to_owned()),
    ]
}

impl EdgeFact {
    /// Rebuild the manifest edge this row stores.
    pub(in crate::engine) fn to_edge(&self) -> Result<DepEdge, OrmError> {
        Ok(DepEdge {
            name: smol_str::SmolStr::new(&self.name),
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

fn edge_hash(requirement: &str, class: &str, optional: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(requirement.as_bytes());
    hasher.update(&[0xff]);
    hasher.update(class.as_bytes());
    hasher.update(&[0xff]);
    hasher.update(optional.as_bytes());
    hasher.finalize().to_hex().to_string()
}
