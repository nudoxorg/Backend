//! Rebuild the in-memory versioned ledger from the SQL catalog tip.
//!
//! The Turso fork keeps rows in memory, so a process start would otherwise
//! open an empty ledger beside a durable catalog. This read copies each
//! version and its edges into [`VersionedCatalog`]. A second pass of the same
//! tip is unchanged.

use std::collections::BTreeMap;

use heart::Language;
use sea_orm::sea_query::{Expr, Query};
use smol_str::SmolStr;

use crate::{
    engine::{CatalogEngine, EngineError},
    entity::{edges, packages, versions},
    enums::{EdgeKind, TextEnum},
    record::{DepClass, DepEdge, PackageRecord},
};

use super::turso_vc::{FactWrite, VersionedCatalog};

/// How many tip rows a rebuild wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdoptReport {
    /// Rows whose payload or edges differed from the ledger tip.
    pub revised: usize,
    /// Rows already matching the ledger.
    pub unchanged: usize,
}

impl VersionedCatalog {
    /// Copy the SQL catalog tip into this ledger.
    pub fn adopt_catalog<E: CatalogEngine>(
        &mut self,
        engine: &E,
    ) -> Result<AdoptReport, EngineError> {
        let versions = version_rows(engine)?;
        let edges = edge_rows(engine)?;
        let mut revised = 0;
        let mut unchanged = 0;
        for row in versions {
            let mut record = row.record;
            if let Some(found) = edges.get(&row.id) {
                record.edges.clone_from(found);
            }
            match self.put_record(&record).map_err(orm_as_engine)? {
                FactWrite::Revised(_) => revised += 1,
                FactWrite::Unchanged => unchanged += 1,
            }
        }
        Ok(AdoptReport { revised, unchanged })
    }
}

struct VersionTip {
    id: String,
    record: PackageRecord,
}

fn version_rows<E: CatalogEngine>(engine: &E) -> Result<Vec<VersionTip>, EngineError> {
    let mut select = Query::select();
    select
        .column((versions::Entity, versions::Column::Id))
        .column((packages::Entity, packages::Column::Ecosystem))
        .column((packages::Entity, packages::Column::NameCanonical))
        .column((versions::Entity, versions::Column::VersionCanonical))
        .column((versions::Entity, versions::Column::YankedUpstream))
        .column((versions::Entity, versions::Column::LicenseSpdx))
        .column((versions::Entity, versions::Column::SourceRev))
        .column((versions::Entity, versions::Column::RegistryChecksum))
        .from(versions::Entity)
        .inner_join(
            packages::Entity,
            Expr::col((versions::Entity, versions::Column::StemId))
                .equals((packages::Entity, packages::Column::StemId)),
        );
    crate::engine::stmt::query_select(engine, select, &mut |row| {
        let id = uuid_text(&row.get_blob(0)?)?;
        let ecosystem = row.get_text(1)?;
        let Some(language) = Language::from_token(&ecosystem) else {
            return Ok(None);
        };
        let mut record = PackageRecord::from_parts(
            language,
            row.get_text(2)?,
            row.get_text(3)?,
            None,
            row.get_optional_text(5)?.map(SmolStr::new),
            Vec::new(),
            None,
            None,
            row.get_integer(4)? != 0,
            Vec::new(),
        );
        if let Some(digest) = crate::pid::observed_content(
            row.get_optional_text(7)?.as_deref(),
            row.get_optional_text(6)?.as_deref(),
        ) {
            record = record.with_content(digest);
        }
        Ok(Some(VersionTip { id, record }))
    })
    .map(|rows| rows.into_iter().flatten().collect())
}

fn edge_rows<E: CatalogEngine>(engine: &E) -> Result<BTreeMap<String, Vec<DepEdge>>, EngineError> {
    let mut select = Query::select();
    select
        .column((edges::Entity, edges::Column::DependentVersion))
        .column((edges::Entity, edges::Column::DepEcosystem))
        .column((edges::Entity, edges::Column::DepNameCanonical))
        .column((edges::Entity, edges::Column::Kind))
        .column((edges::Entity, edges::Column::Requirement))
        .from(edges::Entity);
    let rows = crate::engine::stmt::query_select(engine, select, &mut |row| {
        let id = uuid_text(&row.get_blob(0)?)?;
        let kind = row.get_text(3)?;
        let Ok(kind) = EdgeKind::from_token(&kind) else {
            return Ok(None);
        };
        let class = match kind {
            EdgeKind::Runtime | EdgeKind::Recipe => DepClass::Runtime,
            EdgeKind::Build
            | EdgeKind::FindPackage
            | EdgeKind::PkgConfig
            | EdgeKind::Submodule
            | EdgeKind::FetchContent
            | EdgeKind::Wrap
            | EdgeKind::BazelDep
            | EdgeKind::Vendored => DepClass::Build,
        };
        let dep_ecosystem = row.get_text(1)?;
        let requirement = row.get_text(4)?;
        Ok(Some((id, DepEdge {
            name: SmolStr::new(row.get_text(2)?),
            requirement: if requirement.is_empty() {
                None
            } else {
                Some(SmolStr::new(requirement))
            },
            class,
            optional: false,
            dep_ecosystem: Language::from_token(&dep_ecosystem),
        })))
    })?;
    let mut grouped = BTreeMap::<String, Vec<DepEdge>>::new();
    for (id, edge) in rows.into_iter().flatten() {
        grouped.entry(id).or_default().push(edge);
    }
    Ok(grouped)
}

fn uuid_text(blob: &[u8]) -> Result<String, EngineError> {
    crate::ids::version_id::from_blob(blob)
        .map(|id| id.as_uuid().to_string())
        .map_err(|err| EngineError::Statement(err.to_string()))
}

fn orm_as_engine(error: turso_versioning::orm::OrmError) -> EngineError {
    EngineError::Statement(error.to_string())
}
