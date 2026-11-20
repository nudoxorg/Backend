//! Select admitted Stylo facts from one retained comparison side and feed the
//! bounded candidate walk. No current filesystem lookup is performed here.
use super::impact_retention::{Basis, ChangeSide, Coverage, Lease, LeaseId, LeaseStore};
use super::stylo_impact_walk::{self as walk, Declaration, Dependency, Key, Limits, Owner};
use backend_engine::builtin::{StyloAnalysisRecord, StyloAnalysisRelation, StyloProjectedRow};
use backend_semantic::stylo_analysis::{StyloDeclarationKind, StyloSourceRole};
use backend_version::CanonicalRelation;
use std::collections::BTreeMap;
use std::time::Instant;

#[derive(Debug, Eq, PartialEq)]
pub(super) enum Error {
    Input,
    Cancelled,
    Decode,
    SeedUnavailable,
}

pub(super) struct WalkResult {
    pub outcome: walk::Outcome,
    pub paths: BTreeMap<Key, String>,
    pub input_rows: usize,
    pub input_bytes: usize,
    pub temporary_bytes: usize,
}

/// Uses only rows admitted and copied under the lease's exact native root.
/// Missing rows and all CSS provider/source coverage remain explicitly gapped;
/// no name equality is treated as semantic binding.
pub(super) fn walk_lease<F: FnMut() -> bool>(
    lease: &Lease,
    seed: Key,
    limits: Limits,
    mut cancelled: F,
) -> Result<WalkResult, Error> {
    if lease.records.len() > limits.input_rows {
        return Err(Error::Input);
    }
    let mut input = 0usize;
    let mut temporary = 131_072usize;
    if temporary > limits.temporary_bytes {
        return Err(Error::Input);
    }
    let mut declarations = Vec::new();
    let mut dependencies = Vec::new();
    let mut seed_name = None;
    let mut paths = BTreeMap::new();
    for material in lease.records.iter() {
        if cancelled() {
            return Err(Error::Cancelled);
        }
        input = input
            .checked_add(material.bytes.len())
            .ok_or(Error::Input)?;
        if input > limits.input_bytes {
            return Err(Error::Input);
        }
        temporary = temporary
            .saturating_add(1024)
            .saturating_add(material.bytes.len().saturating_mul(2));
        if temporary > limits.temporary_bytes {
            return Err(Error::Input);
        }
        let row =
            StyloAnalysisRelation::decode_value(&material.bytes).map_err(|_| Error::Decode)?;
        if let StyloAnalysisRecord::Host { ref path, .. } = row {
            paths.insert(material.key, path.clone());
        }
        let StyloAnalysisRecord::Fact { host, fact, .. } = row else {
            continue;
        };
        match fact {
            StyloProjectedRow::SourceOccurrence(site)
                if material.key == seed
                    && site.role == StyloSourceRole::CustomPropertyDeclarationName =>
            {
                seed_name = Some(site.decoded);
            }
            StyloProjectedRow::RetainedDeclaration(declaration) => {
                let custom_property = (declaration.kind == StyloDeclarationKind::Custom
                    && declaration.property.starts_with("--")
                    && !declaration.property.contains('\\'))
                .then_some(declaration.property);
                declarations.push(Declaration {
                    id: material.key,
                    owner: Owner {
                        host,
                        rule: declaration.rule_path,
                        ordinal: declaration.ordinal,
                    },
                    custom_property,
                });
            }
            StyloProjectedRow::Dependency(dependency) if dependency.kind == "var" => {
                dependencies.push(Dependency {
                    id: material.key,
                    owner: Owner {
                        host,
                        rule: dependency.rule_path,
                        ordinal: dependency.declaration_ordinal,
                    },
                    name: dependency.name,
                });
            }
            _ => {}
        }
    }
    let seed_name = seed_name.ok_or(Error::SeedUnavailable)?;
    if cancelled() {
        return Err(Error::Cancelled);
    }
    Ok(WalkResult {
        outcome: walk::walk(
            seed,
            &seed_name,
            &declarations,
            &dependencies,
            limits,
            cancelled,
        ),
        paths,
        input_rows: lease.records.len(),
        input_bytes: input,
        temporary_bytes: temporary,
    })
}

/// One explicit base/target query; both roots and side coverage accompany the
/// result. This chooses a side and does not prove a seed was removed or added;
/// no spelling-based identity continuity is inferred.
#[derive(Debug)]
pub(super) struct ChangeOutcome {
    pub base: Basis,
    pub target: Basis,
    pub side: ChangeSide,
    pub relation_coverage: Coverage,
    pub source_coverage: Coverage,
    pub result: walk::Outcome,
    pub paths: BTreeMap<Key, String>,
    pub input_rows: usize,
    pub input_bytes: usize,
    pub temporary_bytes: usize,
}
#[derive(Debug)]
pub(super) enum ChangeError {
    Retention(super::impact_retention::Error),
    Walk(Error),
}

pub(super) fn walk_change<F: FnMut() -> bool>(
    store: &mut LeaseStore,
    base: LeaseId,
    target: LeaseId,
    side: ChangeSide,
    seed: Key,
    limits: Limits,
    now: Instant,
    cancelled: F,
) -> Result<ChangeOutcome, ChangeError> {
    let (base_basis, target_basis) = store
        .comparison_bases(base, target, now)
        .map_err(ChangeError::Retention)?;
    let lease = store
        .comparison_side(base, target, side, now)
        .map_err(ChangeError::Retention)?;
    let walked = walk_lease(lease, seed, limits, cancelled).map_err(ChangeError::Walk)?;
    Ok(ChangeOutcome {
        base: base_basis,
        target: target_basis,
        side,
        relation_coverage: lease.relation_coverage,
        source_coverage: lease.source_coverage,
        result: walked.outcome,
        paths: walked.paths,
        input_rows: walked.input_rows,
        input_bytes: walked.input_bytes,
        temporary_bytes: walked.temporary_bytes,
    })
}
