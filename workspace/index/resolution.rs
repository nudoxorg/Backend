//! The dependency-edge resolution pass (REGISTRYLESS-PLAN §8, RL-5).
//!
//! Dependency edges are recorded exactly as the extractor saw them (the EDB):
//! a literal `find_package(ZLIB)` token, a `pkg_config: openssl` name, a bare
//! `Threads`. Binding those literal tokens onto canonical package stems is a
//! **derived** fact (the IDB), computed here by consulting `package_aliases`.
//!
//! The pass is:
//! - **pure** — it reads `edges` + `package_aliases` and writes only the
//!   `edges.resolved_stem` column; it invents nothing;
//! - **idempotent / rerunnable** — it only touches edges whose `resolved_stem`
//!   is still `NULL`, and a hit fills exactly the stem the alias table names, so
//!   a second run over the same catalog produces the identical rows;
//! - **never destructive** — an edge that already carries a `resolved_stem` is
//!   left untouched, and an edge with no matching alias stays `NULL`
//!   (`Absent`-tier per the confidence law — never an invented stem).
//!
//! Conflicting aliases (two rows mapping the same `(ecosystem, alias_kind,
//! alias)` triple onto different stems) cannot occur under the table's primary
//! key `(ecosystem, alias_kind, alias)`; the confidence ordering nonetheless
//! matters when the *same dependency token* is reachable under more than one
//! alias kind, or when curation history has left multiple candidate rows a
//! caller passes to [`select_by_confidence`]. Authoritative > curated >
//! heuristic; ties break deterministically on the stem bytes.

use heart::Language;

use crate::codec::CodecError;
use crate::engine::{CatalogEngine, Row, Value};
use crate::enums::{AliasConfidence, EdgeKind, TextEnum};
use crate::ids::PackageStemId;
use crate::store::MetaError;
use crate::tables::{aliases, edges};

/// The alias-kind token a given [`EdgeKind`] resolves through (REGISTRYLESS §8).
///
/// Only the mechanisms that name a *curated* token (a CMake `find_package`
/// argument, a pkg-config module, a meson wrap, a recipe/bazel/vcpkg name) route
/// through the alias table. `submodule` and `fetchcontent` edges already carry a
/// repository-slug token — they are self-resolving and must **not** be looked up
/// as aliases (returning `None` here leaves them for slug-based resolution
/// elsewhere). `runtime`/`build` are generic manifest edges with no C/C++ alias
/// namespace.
///
/// The returned token matches the `package_aliases.alias_kind` vocabulary
/// (the snake_case [`crate::enums`] / `ecosystem::cpp::alias::AliasKind` tokens).
pub fn edge_kind_to_alias_kind(kind: EdgeKind) -> Option<&'static str> {
    match kind {
        // Curated-token mechanisms: resolved via the alias table.
        EdgeKind::FindPackage => Some("find_package"),
        EdgeKind::PkgConfig => Some("pkg_config"),
        EdgeKind::Wrap => Some("meson_wrap"),
        EdgeKind::Recipe => Some("vcpkg_port"),
        EdgeKind::BazelDep => Some("bazel_module"),
        // Self-resolving repo-slug tokens — never alias-looked-up.
        EdgeKind::Submodule | EdgeKind::FetchContent => None,
        // Generic manifest edges / detected vendored copies: no alias namespace.
        EdgeKind::Runtime | EdgeKind::Build | EdgeKind::Vendored => None,
    }
}

/// One `package_aliases` candidate for a dependency token: the stem it names and
/// how much we trust the mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AliasCandidate {
    /// The canonical stem the alias resolves to.
    pub stem: PackageStemId,
    /// How trustworthy the mapping is (authoritative > curated > heuristic).
    pub confidence: AliasConfidence,
}

/// A numeric rank for a confidence tier: higher wins. Authoritative feeds beat
/// curated seed entries, which beat heuristic homepage sniffs (REGISTRYLESS §8).
fn confidence_rank(confidence: AliasConfidence) -> u8 {
    match confidence {
        AliasConfidence::Authoritative => 2,
        AliasConfidence::Curated => 1,
        AliasConfidence::Heuristic => 0,
    }
}

/// Pick the single winning candidate from a set of aliases for one dependency
/// token, deterministically.
///
/// The winner is the highest-confidence candidate; ties (equal confidence) break
/// on the stem's raw bytes so the result never depends on row iteration order.
/// Returns `None` for an empty candidate set (an unresolved edge — left `NULL`).
pub fn select_by_confidence(candidates: &[AliasCandidate]) -> Option<AliasCandidate> {
    candidates
        .iter()
        .copied()
        .max_by(|left, right| {
            // Higher confidence wins; on a tie the *lower* stem bytes win, so we
            // reverse the stem comparison (`max_by` keeps the greater element).
            confidence_rank(left.confidence)
                .cmp(&confidence_rank(right.confidence))
                .then_with(|| right.stem.to_blob().cmp(&left.stem.to_blob()))
        })
}

/// The outcome of one resolution pass, for observability. Every field is a pure
/// count; a rerun over an unchanged catalog reports `resolved == 0`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResolutionReport {
    /// Unresolved edges examined (`resolved_stem IS NULL`, scoped ecosystem).
    pub scanned: usize,
    /// Edges newly bound to a stem this pass.
    pub resolved: usize,
    /// Edges left unresolved (no matching alias, or a non-alias mechanism).
    pub unresolved: usize,
}

/// An unresolved edge read back for resolution: enough of its primary key to
/// write the fill back, plus the token + kind to look up.
struct UnresolvedEdge {
    dependent_version: Vec<u8>,
    dep_ecosystem_token: String,
    dep_name_canonical: String,
    kind: EdgeKind,
}

/// Run the resolution pass for one ecosystem, filling `resolved_stem` on every
/// currently-unresolved edge that a `package_aliases` entry can bind.
///
/// Pure, idempotent, rerunnable (see the module docs). All writes for the batch
/// run in **one** transaction so a partial failure leaves no half-resolved
/// catalog; durability is the caller's explicit
/// [`commit_batch`](crate::store::writer::CatalogWriter::commit_batch) heartbeat,
/// exactly like [`apply_ops`](crate::store::MetaStore::apply_ops).
///
/// The read (unresolved edges + their candidate aliases) happens before the
/// write transaction; because the write only ever sets a column that was `NULL`,
/// interleaving with the single writer cannot corrupt a row.
pub fn resolve_unresolved_edges<E: CatalogEngine>(
    engine: &E,
    ecosystem: Language,
) -> Result<ResolutionReport, MetaError> {
    let ecosystem_token = ecosystem.as_token().to_owned();

    // ── 1. Read every unresolved edge in this ecosystem ──────────────────────
    let unresolved = read_unresolved_edges(engine, &ecosystem_token)?;

    // ── 2. For each, look up its winning alias (if any) ──────────────────────
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
        // The alias table stores tokens lowercased for case-insensitive lookup;
        // the extractor writes `dep_name_canonical` already canonicalized, but we
        // lowercase here too so the join is total regardless of the extractor's
        // casing discipline.
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

    // ── 3. Write the fills in one transaction ────────────────────────────────
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

/// Read every edge whose `resolved_stem` is `NULL` in the given ecosystem.
fn read_unresolved_edges<E: CatalogEngine>(
    engine: &E,
    ecosystem_token: &str,
) -> Result<Vec<UnresolvedEdge>, MetaError> {
    let sql = format!(
        "SELECT {dv},{de},{dn},{kind} FROM {table} \
         WHERE {de} = ?1 AND {rs} IS NULL",
        dv = edges::columns::DEPENDENT_VERSION,
        de = edges::columns::DEP_ECOSYSTEM,
        dn = edges::columns::DEP_NAME_CANONICAL,
        kind = edges::columns::KIND,
        rs = edges::columns::RESOLVED_STEM,
        table = edges::TABLE,
    );
    let rows = engine.query_rows(
        &sql,
        &[Value::Text(ecosystem_token.to_owned())],
        &mut |row: &dyn Row| {
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
        },
    )?;
    Ok(rows)
}

/// Read the candidate aliases for one `(ecosystem, alias_kind, alias)` lookup.
fn read_alias_candidates<E: CatalogEngine>(
    engine: &E,
    ecosystem_token: &str,
    alias_kind: &str,
    alias_token: &str,
) -> Result<Vec<AliasCandidate>, MetaError> {
    let sql = format!(
        "SELECT {stem},{confidence} FROM {table} \
         WHERE {eco} = ?1 AND {ak} = ?2 AND {al} = ?3",
        stem = aliases::columns::STEM_ID,
        confidence = aliases::columns::CONFIDENCE,
        table = aliases::TABLE,
        eco = aliases::columns::ECOSYSTEM,
        ak = aliases::columns::ALIAS_KIND,
        al = aliases::columns::ALIAS,
    );
    let rows = engine.query_rows(
        &sql,
        &[
            Value::Text(ecosystem_token.to_owned()),
            Value::Text(alias_kind.to_owned()),
            Value::Text(alias_token.to_owned()),
        ],
        &mut |row: &dyn Row| {
            let stem = PackageStemId::from_blob(&row.get_blob(0)?)
                .map_err(CodecError::from)
                .map_err(engine_error_from_codec)?;
            let confidence_token = row.get_text(1)?;
            let confidence = AliasConfidence::from_token(&confidence_token)
                .map_err(CodecError::from)
                .map_err(engine_error_from_codec)?;
            Ok(AliasCandidate { stem, confidence })
        },
    )?;
    Ok(rows)
}

/// Fill `resolved_stem` for one edge, but only while it is still `NULL`.
///
/// The `AND resolved_stem IS NULL` guard makes the write idempotent and
/// non-destructive at the SQL level: a concurrent writer that resolved the edge
/// first wins, and a rerun is a no-op.
fn fill_resolved_stem(
    tx: &dyn CatalogEngine,
    edge: &UnresolvedEdge,
    stem: PackageStemId,
) -> Result<(), MetaError> {
    let sql = format!(
        "UPDATE {table} SET {rs} = ?1 \
         WHERE {dv} = ?2 AND {de} = ?3 AND {dn} = ?4 AND {kind} = ?5 \
         AND {rs} IS NULL",
        table = edges::TABLE,
        rs = edges::columns::RESOLVED_STEM,
        dv = edges::columns::DEPENDENT_VERSION,
        de = edges::columns::DEP_ECOSYSTEM,
        dn = edges::columns::DEP_NAME_CANONICAL,
        kind = edges::columns::KIND,
    );
    tx.execute(
        &sql,
        &[
            Value::Blob(stem.to_blob().to_vec()),
            Value::Blob(edge.dependent_version.clone()),
            Value::Text(edge.dep_ecosystem_token.clone()),
            Value::Text(edge.dep_name_canonical.clone()),
            Value::Text(edge.kind.as_token().to_owned()),
        ],
    )?;
    Ok(())
}

/// Lift a [`CodecError`] surfaced inside a row-mapping closure into the
/// [`EngineError`] channel the facade closures speak.
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
        assert_eq!(edge_kind_to_alias_kind(EdgeKind::FindPackage), Some("find_package"));
        assert_eq!(edge_kind_to_alias_kind(EdgeKind::PkgConfig), Some("pkg_config"));
        // Self-resolving slug mechanisms are never alias-looked-up.
        assert_eq!(edge_kind_to_alias_kind(EdgeKind::Submodule), None);
        assert_eq!(edge_kind_to_alias_kind(EdgeKind::FetchContent), None);
        // Generic manifest edges have no C/C++ alias namespace.
        assert_eq!(edge_kind_to_alias_kind(EdgeKind::Runtime), None);
    }

    #[test]
    fn select_by_confidence_prefers_higher_tier() {
        let curated = AliasCandidate { stem: stem(1), confidence: AliasConfidence::Curated };
        let authoritative =
            AliasCandidate { stem: stem(2), confidence: AliasConfidence::Authoritative };
        let heuristic = AliasCandidate { stem: stem(3), confidence: AliasConfidence::Heuristic };
        let winner = select_by_confidence(&[curated, heuristic, authoritative])
            .expect("non-empty set resolves");
        assert_eq!(winner, authoritative, "authoritative outranks curated and heuristic");
    }

    #[test]
    fn select_by_confidence_breaks_ties_deterministically() {
        // Two curated candidates onto different stems: the lower stem bytes win,
        // and the choice is order-independent.
        let low = AliasCandidate { stem: stem(1), confidence: AliasConfidence::Curated };
        let high = AliasCandidate { stem: stem(9), confidence: AliasConfidence::Curated };
        let forward = select_by_confidence(&[low, high]).expect("resolves");
        let reversed = select_by_confidence(&[high, low]).expect("resolves");
        assert_eq!(forward, reversed, "tie-break must be order-independent");
        assert_eq!(forward, low, "the lower stem bytes win the tie");
    }

    #[test]
    fn select_by_confidence_empty_is_none() {
        assert_eq!(select_by_confidence(&[]), None);
    }
}
