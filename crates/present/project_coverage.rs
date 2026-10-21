//! Exact project-scoped semantic coverage selected for one current view.
//!
//! Workspace health answers whether any project has a lane gap. It cannot
//! attribute that gap to a project. This module joins the owner-selected
//! package-profile receipt to one package row and the exact view revision
//! before presentation is allowed to add a project-level lane tag.

use crate::compiler_causes::snapshot_matches_health;
use crate::drive::Engine;
use backend_library::{
    Coverage, HealthReport, IndexOperationSemanticCoverage, IndexOperationSemanticProfileState,
    PackageCompilerOutcomeBasis, PackageCompilerOutcomeRead, PackageCompilerOutcomes,
    PackageCoverageBinding, PackageCoverageCurrentResult, PackageKey, PackageReference, RowId,
    RowState, SurfaceCommand, SurfaceReply, ViewSnapshot,
};
use std::collections::BTreeMap;

/// One owner-authenticated package coverage receipt, joined to a current view.
///
/// `source_capture` retains the selected profile generations and their exact
/// source/input/observation tuples. `basis` names the package and selected
/// source/capture roots. `view_root` is the product view which the package row
/// came from; callers discard the whole set if the owner advances mid-read.
#[derive(Clone, Debug)]
pub(crate) struct ProjectCoverageEvidence {
    view_root: backend_library::ViewStateRoot,
    package_key: PackageKey,
    package: PackageReference,
    basis: PackageCompilerOutcomeBasis,
    binding: PackageCoverageBinding,
    source_capture: backend_library::IndexSourceCaptureSummary,
    semantic_partial: bool,
}

impl ProjectCoverageEvidence {
    fn from_owner_reply(
        view_root: backend_library::ViewStateRoot,
        expected_source_root: [u8; 32],
        package_key: PackageKey,
        package: &PackageReference,
        result: PackageCoverageCurrentResult,
    ) -> Option<Self> {
        let read = PackageCompilerOutcomeRead::Current;
        result.admit_request(package, *view_root.as_bytes()).ok()?;
        let PackageCoverageCurrentResult::Current { binding, outcomes } = result else {
            return None;
        };
        outcomes.admit_request(package, Some(&read)).ok()?;
        let PackageCompilerOutcomes::Summary {
            basis,
            source_capture,
            ..
        } = outcomes
        else {
            return None;
        };
        if !basis.has_admissible_shape()
            || basis.package_key != package_key.to_bytes()
            || binding.package_key != package_key.to_bytes()
            || binding.view_root != *view_root.as_bytes()
            || binding.source_relation_root != expected_source_root
            || basis.source_relation_root != binding.source_relation_root
            || basis.capture_relation_root != binding.capture_relation_root
            || source_capture.producer_package != *package
            || source_capture
                .profiles
                .iter()
                .any(|profile| profile.source_version != basis.source_version)
        {
            return None;
        }

        let mut selected = 0_usize;
        let mut partial = false;
        let mut refused = false;
        let mut pending = false;
        for profile in source_capture.profiles.iter() {
            match profile.state {
                IndexOperationSemanticProfileState::Published { coverage, .. } => {
                    selected += 1;
                    partial |= matches!(coverage, IndexOperationSemanticCoverage::Partial { .. });
                }
                IndexOperationSemanticProfileState::Unavailable { .. }
                | IndexOperationSemanticProfileState::Failed { .. } => refused = true,
                IndexOperationSemanticProfileState::Pending { .. } => pending = true,
            }
        }

        // A refused profile is a settled semantic gap only when another
        // profile actually published a current generation. Pending work and
        // an unavailable-only project are not reported as partial.
        let semantic_partial = !pending && selected > 0 && (partial || refused);
        Some(Self {
            view_root,
            package_key,
            package: source_capture.producer_package.clone(),
            basis,
            binding,
            source_capture,
            semantic_partial,
        })
    }

    pub(crate) fn matches(
        &self,
        view_root: backend_library::ViewStateRoot,
        source_root: [u8; 32],
        key: PackageKey,
        package: &PackageReference,
    ) -> bool {
        self.view_root == view_root
            && self.basis.source_relation_root == source_root
            && self.binding.view_root == *view_root.as_bytes()
            && self.binding.source_relation_root == source_root
            && self.package_key == key
            && self.package == *package
            && self.basis.package_key == key.to_bytes()
            && self.source_capture.producer_package == *package
    }

    pub(crate) const fn semantic_partial(&self) -> bool {
        self.semantic_partial
    }
}

/// Reads current selected profile summaries only when workspace health says
/// there is a settled semantic gap to attribute. Any view advance invalidates
/// the complete enrichment, so no package can receive another revision's
/// profile evidence.
pub(crate) fn read_current_project_coverage(
    engine: &mut dyn Engine,
    snapshot: &ViewSnapshot,
    health: &HealthReport,
) -> BTreeMap<PackageKey, ProjectCoverageEvidence> {
    let mut result = BTreeMap::new();
    if !snapshot_matches_health(snapshot, health)
        || !health.coverage().iter().any(|coverage| {
            matches!(
                coverage,
                Coverage::Gapped {
                    lane: backend_library::Lane::Semantic
                }
            )
        })
        || engine.revision().ok() != Some(health.revision().root())
    {
        return result;
    }

    let view_root = health.revision().root();
    let source_root = *health.basis().root.as_bytes();
    let ready_packages = snapshot
        .root
        .rows()
        .iter()
        .filter_map(|row| match (row.id, row.state) {
            (RowId::Package(key), RowState::Ready) => Some((key, row.label.as_str())),
            _ => None,
        })
        .collect::<Vec<_>>();
    if ready_packages.len() <= 1 {
        return result;
    }
    let mut selected_relation_roots = None;

    for (key, label) in ready_packages {
        let Ok(package) = PackageReference::parse(label.to_owned()) else {
            continue;
        };
        if !matches!(package, PackageReference::Local(_)) {
            continue;
        }
        let reply = engine.surface(SurfaceCommand::PackageCoverageCurrent {
            package: package.clone(),
            view_root: *view_root.as_bytes(),
        });
        let Some(coverage) = reply.ok().and_then(package_coverage) else {
            continue;
        };
        let Some(evidence) = ProjectCoverageEvidence::from_owner_reply(
            view_root,
            source_root,
            key,
            &package,
            coverage,
        ) else {
            continue;
        };
        let relation_roots = (
            evidence.binding.source_relation_root,
            evidence.binding.semantic_relation_root,
            evidence.binding.capture_relation_root,
        );
        if selected_relation_roots
            .replace(relation_roots)
            .is_some_and(|previous| previous != relation_roots)
        {
            // The owner changed one of the selected publication relations
            // between package reads. Do not combine separate snapshots.
            return BTreeMap::new();
        }
        result.insert(key, evidence);
        if engine.revision().ok() != Some(view_root) {
            return BTreeMap::new();
        }
    }

    if engine.revision().ok() != Some(view_root) {
        return BTreeMap::new();
    }
    result
}

fn package_coverage(reply: SurfaceReply) -> Option<PackageCoverageCurrentResult> {
    match reply {
        SurfaceReply::PackageCoverageCurrent(result) => Some(result),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_library::{
        Basis, CapabilityInventory, Command, CommandReply, Cursor, Frontier, HealthReport,
        IndexOperationSemanticCoverage, IndexOperationSemanticProfileState,
        IndexOperationSourceProfile, IndexSourceCaptureSummary, Lane, Library,
        PackageCompilerOutcomeBasis, PackageCompilerOutcomes, RevisionReceipt, Row, ViewRoot,
        object_version, package_key, view_key, view_state_root,
    };
    use backend_semantic::vocabulary::{LanguageProfile, TypeScriptSource};

    const ASP: &str = "/workspace/asp";
    const GBA: &str = "/workspace/gba";

    struct ShelfRevision {
        library: Library,
        health: HealthReport,
    }

    fn revision(
        paths: &[&str],
        generation: u8,
    ) -> Result<ShelfRevision, Box<dyn std::error::Error>> {
        let basis = Basis::new(
            view_state_root(&[("workspace".to_owned(), format!("revision-{generation}"))]),
            object_version(b"project coverage source"),
        );
        let rows = paths
            .iter()
            .map(|path| {
                let package = package_key(path);
                Row::in_package(RowId::Package(package), basis, package, *path)
            })
            .collect();
        let root = ViewRoot::new_incomplete(
            view_key(b"project coverage shelf"),
            basis,
            Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 1),
            rows,
            Vec::new(),
        )?;
        let cursor = Cursor::for_view_root(&root);
        let library = Library::from_view(root.clone(), cursor)?;
        let health = HealthReport::from_admitted_parts(
            RevisionReceipt::new(root.root(), cursor, root.basis().object),
            root.basis(),
            vec![
                Coverage::Complete,
                Coverage::Gapped {
                    lane: Lane::Semantic,
                },
            ]
            .into_boxed_slice(),
            root.row_count(),
            CapabilityInventory::explicitly_unavailable(),
        );
        Ok(ShelfRevision { library, health })
    }

    fn snapshot(revision: &ShelfRevision) -> Result<ViewSnapshot, Box<dyn std::error::Error>> {
        let Ok(CommandReply::Packages(snapshot)) = revision.library.execute(Command::Packages)
        else {
            return Err("library did not return its package projection".into());
        };
        Ok(snapshot)
    }

    fn owner_summary(
        package: &PackageReference,
        view_root: backend_library::ViewStateRoot,
        source_root: [u8; 32],
        source_version: [u8; 32],
        generation: u8,
        partial: bool,
    ) -> PackageCoverageCurrentResult {
        let profile = backend_library::SemanticLanguageProfile::new(LanguageProfile::TypeScript(
            TypeScriptSource::TypeScript,
        ));
        let outcomes = PackageCompilerOutcomes::Summary {
            coverage: None,
            basis: PackageCompilerOutcomeBasis {
                package_key: package_key(package.as_str()).to_bytes(),
                source_relation_root: source_root,
                source_version,
                capture_relation_root: [90; 32],
            },
            source_capture: IndexSourceCaptureSummary {
                producer_package: package.clone(),
                request_identity: [generation.wrapping_add(2); 32],
                commit_identity: [generation.wrapping_add(3); 32],
                workspace_root: [generation.wrapping_add(4); 32],
                workspace_sequence: u64::from(generation).max(1),
                profiles: vec![IndexOperationSourceProfile {
                    profile,
                    source_version,
                    input_digest: [generation.wrapping_add(5); 32],
                    observation_sequence: u64::from(generation).max(1),
                    source_count: 2,
                    state: IndexOperationSemanticProfileState::Published {
                        generation: [generation; 32],
                        coverage: if partial {
                            IndexOperationSemanticCoverage::Partial {
                                completed: 1,
                                total: 2,
                            }
                        } else {
                            IndexOperationSemanticCoverage::Complete
                        },
                    },
                }]
                .into_boxed_slice(),
            },
            refusals: Box::new([]),
        };
        PackageCoverageCurrentResult::Current {
            binding: PackageCoverageBinding {
                package_key: package_key(package.as_str()).to_bytes(),
                view_root: *view_root.as_bytes(),
                source_relation_root: source_root,
                semantic_relation_root: [91; 32],
                capture_relation_root: [90; 32],
            },
            outcomes,
        }
    }

    fn evidence(
        view_root: backend_library::ViewStateRoot,
        source_root: [u8; 32],
        package: &str,
        source_version: [u8; 32],
        generation: u8,
        partial: bool,
    ) -> Result<ProjectCoverageEvidence, Box<dyn std::error::Error>> {
        let package = PackageReference::parse(package.to_owned())?;
        let key = package_key(package.as_str());
        ProjectCoverageEvidence::from_owner_reply(
            view_root,
            source_root,
            key,
            &package,
            owner_summary(
                &package,
                view_root,
                source_root,
                source_version,
                generation,
                partial,
            ),
        )
        .ok_or_else(|| "owner profile summary did not match the exact request".into())
    }

    struct ProfileEngine {
        revision: backend_library::ViewStateRoot,
        outcomes: BTreeMap<PackageKey, PackageCoverageCurrentResult>,
        advance_after_read: bool,
        reads: usize,
    }

    impl Engine for ProfileEngine {
        fn revision(
            &mut self,
        ) -> Result<backend_library::ViewStateRoot, backend_client::ClientError> {
            Ok(self.revision)
        }

        fn health(&mut self) -> Result<HealthReport, backend_client::ClientError> {
            Err(backend_client::ClientError::Protocol(
                "health is not queried in the current coverage test".to_owned(),
            ))
        }

        fn probe(
            &mut self,
            _probe: crate::Probe<'_>,
        ) -> Result<backend_library::ReplyDto, backend_client::ClientError> {
            Err(backend_client::ClientError::Protocol(
                "probe is not queried in the current coverage test".to_owned(),
            ))
        }

        fn surface(
            &mut self,
            command: SurfaceCommand,
        ) -> Result<SurfaceReply, backend_client::ClientError> {
            let SurfaceCommand::PackageCoverageCurrent { package, view_root } = command else {
                return Err(backend_client::ClientError::Protocol(
                    "expected package-coverage request".to_owned(),
                ));
            };
            assert_eq!(view_root, *self.revision.as_bytes());
            self.reads += 1;
            if self.advance_after_read && self.reads == 1 {
                self.revision = backend_library::view_state_root(&[(
                    "workspace".to_owned(),
                    "advanced during profile reads".to_owned(),
                )]);
            }
            let Some(outcomes) = self.outcomes.get(&package_key(package.as_str())).cloned() else {
                return Err(backend_client::ClientError::Protocol(
                    "missing owner-selected profile receipt".to_owned(),
                ));
            };
            Ok(SurfaceReply::PackageCoverageCurrent(outcomes))
        }
    }

    #[test]
    fn two_project_shelf_attributes_only_each_current_profiles_semantic_gap()
    -> Result<(), Box<dyn std::error::Error>> {
        let revision = revision(&[ASP, GBA], 11)?;
        let snapshot = snapshot(&revision)?;
        let view_root = revision.health.revision().root();
        let source_root = *revision.health.basis().root.as_bytes();
        let source_version = [12; 32];
        let asp = evidence(view_root, source_root, ASP, source_version, 13, true)?;
        let gba = evidence(view_root, source_root, GBA, source_version, 14, false)?;
        let evidence = BTreeMap::from([(package_key(ASP), asp), (package_key(GBA), gba)]);

        let shelf = crate::assemble::shelf_from_snapshot_with_project_coverage(
            &snapshot,
            crate::KeyTag::from_key(&[15; 32]),
            &revision.health,
            &evidence,
        );
        let asp = shelf
            .entries()
            .iter()
            .find(|entry| entry.identity().coordinate().as_str() == ASP)
            .ok_or("missing ASP shelf entry")?;
        let gba = shelf
            .entries()
            .iter()
            .find(|entry| entry.identity().coordinate().as_str() == GBA)
            .ok_or("missing GBA shelf entry")?;
        assert_eq!(asp.partial_lanes(), &[Lane::Semantic]);
        assert!(gba.partial_lanes().is_empty());
        Ok(())
    }

    #[test]
    fn current_owner_reads_join_each_package_and_discard_if_view_moves()
    -> Result<(), Box<dyn std::error::Error>> {
        let revision = revision(&[ASP, GBA], 41)?;
        let snapshot = snapshot(&revision)?;
        let view_root = revision.health.revision().root();
        let source_root = *revision.health.basis().root.as_bytes();
        let source_version = [42; 32];
        let outcomes = BTreeMap::from([
            (
                package_key(ASP),
                owner_summary(
                    &PackageReference::parse(ASP)?,
                    view_root,
                    source_root,
                    source_version,
                    43,
                    true,
                ),
            ),
            (
                package_key(GBA),
                owner_summary(
                    &PackageReference::parse(GBA)?,
                    view_root,
                    source_root,
                    source_version,
                    44,
                    false,
                ),
            ),
        ]);
        let mut engine = ProfileEngine {
            revision: view_root,
            outcomes: outcomes.clone(),
            advance_after_read: false,
            reads: 0,
        };
        let evidence = read_current_project_coverage(&mut engine, &snapshot, &revision.health);
        assert_eq!(evidence.len(), 2);
        assert_eq!(engine.reads, 2);

        let mut advancing_engine = ProfileEngine {
            revision: view_root,
            outcomes,
            advance_after_read: true,
            reads: 0,
        };
        assert!(
            read_current_project_coverage(&mut advancing_engine, &snapshot, &revision.health)
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn single_project_shelf_does_not_issue_owner_coverage_reads()
    -> Result<(), Box<dyn std::error::Error>> {
        let revision = revision(&[ASP], 51)?;
        let snapshot = snapshot(&revision)?;
        let mut engine = ProfileEngine {
            revision: revision.health.revision().root(),
            outcomes: BTreeMap::new(),
            advance_after_read: false,
            reads: 0,
        };
        assert!(read_current_project_coverage(&mut engine, &snapshot, &revision.health).is_empty());
        assert_eq!(engine.reads, 0);
        Ok(())
    }

    #[test]
    fn stale_view_or_package_substitution_cannot_reuse_a_project_receipt()
    -> Result<(), Box<dyn std::error::Error>> {
        let old = revision(&[ASP, GBA], 21)?;
        let old_view = old.health.revision().root();
        let old_source = *old.health.basis().root.as_bytes();
        let old_source_version = [22; 32];
        let asp_evidence = evidence(old_view, old_source, ASP, old_source_version, 23, true)?;

        let current = revision(&[ASP, GBA], 31)?;
        let current_snapshot = snapshot(&current)?;
        let asp_key = package_key(ASP);
        let gba_key = package_key(GBA);
        let current_source = *current.health.basis().root.as_bytes();
        let current_view = current.health.revision().root();
        let current_source_version = [32; 32];
        let stale_view_map = BTreeMap::from([(asp_key, asp_evidence.clone())]);
        let stale_source = evidence(current_view, old_source, ASP, old_source_version, 24, true)?;
        let substituted_package = evidence(
            current_view,
            current_source,
            ASP,
            current_source_version,
            25,
            true,
        )?;

        for map in [
            stale_view_map,
            BTreeMap::from([(asp_key, stale_source)]),
            BTreeMap::from([(gba_key, substituted_package)]),
        ] {
            let shelf = crate::assemble::shelf_from_snapshot_with_project_coverage(
                &current_snapshot,
                crate::KeyTag::from_key(&[33; 32]),
                &current.health,
                &map,
            );
            assert!(
                shelf
                    .entries()
                    .iter()
                    .all(|entry| entry.partial_lanes().is_empty())
            );
        }
        Ok(())
    }
}
