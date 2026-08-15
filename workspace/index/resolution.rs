//! The dependency-edge resolution pass (REGISTRYLESS-PLAN §8, RL-5).
//!
//! Reads and writes go through SeaORM entity queries / `update_many`.

use sea_orm::sea_query::Expr;
use sea_orm::{
    ColumnTrait, Condition, DbBackend, EntityTrait, QueryFilter, QuerySelect, QueryTrait,
};

use heart::Language;

use crate::codec::CodecError;
use crate::engine::{self, CatalogEngine, Row};
use crate::entity::{edges, package_aliases};
use crate::enums::{AliasConfidence, EdgeKind, TextEnum};
use crate::ids::PackageStemId;
use crate::store::MetaError;

/// The alias-kind token a given [`EdgeKind`] resolves through (REGISTRYLESS §8).
pub fn edge_kind_to_alias_kind(kind: EdgeKind) -> Option<&'static str> {
    match kind {
        EdgeKind::FindPackage => Some("find_package"),
        EdgeKind::PkgConfig => Some("pkg_config"),
        EdgeKind::Wrap => Some("meson_wrap"),
        EdgeKind::Recipe => Some("vcpkg_port"),
        EdgeKind::BazelDep => Some("bazel_module"),
        EdgeKind::Submodule
        | EdgeKind::FetchContent
        | EdgeKind::Runtime
        | EdgeKind::Build
        | EdgeKind::Vendored => None,
    }
}

/// One `package_aliases` candidate for a dependency token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AliasCandidate {
    pub stem: PackageStemId,
    pub confidence: AliasConfidence,
}

fn confidence_rank(confidence: AliasConfidence) -> u8 {
    match confidence {
        AliasConfidence::Authoritative => 2,
        AliasConfidence::Curated => 1,
        AliasConfidence::Heuristic => 0,
    }
}

/// Pick the single winning candidate deterministically.
pub fn select_by_confidence(candidates: &[AliasCandidate]) -> Option<AliasCandidate> {
    candidates.iter().copied().max_by(|left, right| {
        confidence_rank(left.confidence)
            .cmp(&confidence_rank(right.confidence))
            .then_with(|| right.stem.to_blob().cmp(&left.stem.to_blob()))
    })
}

/// Outcome of one resolution pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResolutionReport {
    pub scanned: usize,
    pub resolved: usize,
    pub unresolved: usize,
}

struct UnresolvedEdge {
    dependent_version: Vec<u8>,
    dep_ecosystem_token: String,
    dep_name_canonical: String,
    kind: EdgeKind,
}

/// Run the resolution pass for one ecosystem.
pub fn resolve_unresolved_edges<E: CatalogEngine>(
    engine: &E,
    ecosystem: Language,
) -> Result<ResolutionReport, MetaError> {
    let ecosystem_token = ecosystem.as_token().to_owned();
    let unresolved = read_unresolved_edges(engine, &ecosystem_token)?;

    let mut fills: Vec<(UnresolvedEdge, PackageStemId)> = Vec::new();
    let mut report = ResolutionReport {
        scanned: unresolved.len(),
        ..ResolutionReport::default()
    };
    for edge in unresolved {
        let Some(alias_kind) = edge_kind_to_alias_kind(edge.kind) else {
            report.unresolved += 1;
            continue;
        };
        let token = edge.dep_name_canonical.to_ascii_lowercase();
        let candidates = read_alias_candidates(engine, &ecosystem_token, alias_kind, &token)?;
        match select_by_confidence(&candidates) {
            Some(winner) => fills.push((edge, winner.stem)),
            None => report.unresolved += 1,
        }
    }

    if fills.is_empty() {
        return Ok(report);
    }

    let mut deferred: Option<MetaError> = None;
    let transaction_result = engine.transaction(&mut |tx| {
        for (edge, stem) in &fills {
            if let Err(error) = fill_resolved_stem(tx, edge, *stem) {
                let engine_error = match error {
                    MetaError::Engine(inner) => inner,
                    other => {
                        let message = other.to_string();
                        deferred = Some(other);
                        crate::engine::EngineError::Statement(message)
                    }
                };
                return Err(engine_error);
            }
        }
        Ok(())
    });

    match transaction_result {
        Ok(()) => {
            report.resolved = fills.len();
            Ok(report)
        }
        Err(engine_error) => Err(deferred.unwrap_or(MetaError::Engine(engine_error))),
    }
}

fn read_unresolved_edges<E: CatalogEngine>(
    engine: &E,
    ecosystem_token: &str,
) -> Result<Vec<UnresolvedEdge>, MetaError> {
    let stmt = edges::Entity::find()
        .filter(
            Condition::all()
                .add(edges::Column::DepEcosystem.eq(ecosystem_token))
                .add(edges::Column::ResolvedStem.is_null()),
        )
        .select_only()
        .column(edges::Column::DependentVersion)
        .column(edges::Column::DepEcosystem)
        .column(edges::Column::DepNameCanonical)
        .column(edges::Column::Kind)
        .build(DbBackend::Sqlite);
    let rows = engine::query(engine, stmt, &mut |row: &dyn Row| {
        let kind_token = row.get_text(3)?;
        let kind = EdgeKind::from_token(&kind_token)
            .map_err(CodecError::from)
            .map_err(engine_error_from_codec)?;
        Ok(UnresolvedEdge {
            dependent_version: row.get_blob(0)?,
            dep_ecosystem_token: row.get_text(1)?,
            dep_name_canonical: row.get_text(2)?,
            kind,
        })
    })?;
    Ok(rows)
}

fn read_alias_candidates<E: CatalogEngine>(
    engine: &E,
    ecosystem_token: &str,
    alias_kind: &str,
    alias_token: &str,
) -> Result<Vec<AliasCandidate>, MetaError> {
    let stmt = package_aliases::Entity::find()
        .filter(
            Condition::all()
                .add(package_aliases::Column::Ecosystem.eq(ecosystem_token))
                .add(package_aliases::Column::AliasKind.eq(alias_kind))
                .add(package_aliases::Column::Alias.eq(alias_token)),
        )
        .select_only()
        .column(package_aliases::Column::StemId)
        .column(package_aliases::Column::Confidence)
        .build(DbBackend::Sqlite);
    let rows = engine::query(engine, stmt, &mut |row: &dyn Row| {
        let stem = PackageStemId::from_blob(&row.get_blob(0)?)
            .map_err(CodecError::from)
            .map_err(engine_error_from_codec)?;
        let confidence_token = row.get_text(1)?;
        let confidence = AliasConfidence::from_token(&confidence_token)
            .map_err(CodecError::from)
            .map_err(engine_error_from_codec)?;
        Ok(AliasCandidate { stem, confidence })
    })?;
    Ok(rows)
}

fn fill_resolved_stem(
    tx: &dyn CatalogEngine,
    edge: &UnresolvedEdge,
    stem: PackageStemId,
) -> Result<(), MetaError> {
    // Dependent version is stored as Uuid blob in the entity.
    let version_uuid = uuid::Uuid::from_slice(&edge.dependent_version).map_err(|e| {
        MetaError::Codec(CodecError::Json(format!("bad dependent_version blob: {e}")))
    })?;

    let stmt = edges::Entity::update_many()
        .col_expr(edges::Column::ResolvedStem, Expr::value(stem))
        .filter(
            Condition::all()
                .add(edges::Column::DependentVersion.eq(version_uuid))
                .add(edges::Column::DepEcosystem.eq(edge.dep_ecosystem_token.as_str()))
                .add(edges::Column::DepNameCanonical.eq(edge.dep_name_canonical.as_str()))
                .add(edges::Column::Kind.eq(edge.kind))
                .add(edges::Column::ResolvedStem.is_null()),
        )
        .build(DbBackend::Sqlite);
    engine::exec(tx, stmt)?;
    Ok(())
}

fn engine_error_from_codec(error: CodecError) -> crate::engine::EngineError {
    crate::engine::EngineError::Statement(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stem(seed: u8) -> PackageStemId {
        let mut bytes = [0u8; 16];
        bytes[0] = seed;
        PackageStemId::from_uuid(uuid::Uuid::from_bytes(bytes))
    }

    #[test]
    fn alias_kind_map_routes_curated_mechanisms_only() {
        assert_eq!(
            edge_kind_to_alias_kind(EdgeKind::FindPackage),
            Some("find_package")
        );
        assert_eq!(
            edge_kind_to_alias_kind(EdgeKind::PkgConfig),
            Some("pkg_config")
        );
        assert_eq!(edge_kind_to_alias_kind(EdgeKind::Submodule), None);
        assert_eq!(edge_kind_to_alias_kind(EdgeKind::FetchContent), None);
        assert_eq!(edge_kind_to_alias_kind(EdgeKind::Runtime), None);
    }

    #[test]
    fn select_by_confidence_prefers_higher_tier() {
        let curated = AliasCandidate {
            stem: stem(1),
            confidence: AliasConfidence::Curated,
        };
        let authoritative = AliasCandidate {
            stem: stem(2),
            confidence: AliasConfidence::Authoritative,
        };
        let heuristic = AliasCandidate {
            stem: stem(3),
            confidence: AliasConfidence::Heuristic,
        };
        let winner = select_by_confidence(&[curated, heuristic, authoritative])
            .expect("non-empty set resolves");
        assert_eq!(
            winner, authoritative,
            "authoritative outranks curated and heuristic"
        );
    }

    #[test]
    fn select_by_confidence_breaks_ties_deterministically() {
        let low = AliasCandidate {
            stem: stem(1),
            confidence: AliasConfidence::Curated,
        };
        let high = AliasCandidate {
            stem: stem(9),
            confidence: AliasConfidence::Curated,
        };
        let forward = select_by_confidence(&[low, high]).expect("resolves");
        let reversed = select_by_confidence(&[high, low]).expect("resolves");
        assert_eq!(forward, reversed);
        assert_eq!(forward.stem, stem(1));
    }

    #[test]
    fn select_by_confidence_empty_is_none() {
        assert_eq!(select_by_confidence(&[]), None);
    }
}
