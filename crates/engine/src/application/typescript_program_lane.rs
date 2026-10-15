//! Exclusive native TypeScript authority retained by one compiler worker lane.
//!
//! The host re-admits the complete forest and source closure on every request.
//! Only that freshly admitted forest may borrow these authorities; this cache
//! never discovers inputs, admits membership, or supplies a closure witness.
//! One package root is resident at a time, and removed/solution nodes are evicted.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use backend_frontend_typescript::{TszProject, TszProjectAuthority, TszUpdateReport};

/// Stable node identity within a package root. The full semantic identity is
/// checked by `TszProjectAuthority::update_with_checker_execution`, including
/// options, library bytes, module outcomes, file formats and source digests.
pub(super) type TypeScriptProgramKey = Option<String>;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct TypeScriptProgramWork {
    pub parsed_and_bound: u64,
    pub reused_binds: u64,
    pub checked_programs: u64,
    pub reused_checked_programs: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TypeScriptProgramLaneError {
    DuplicateProgramIdentity,
}

#[derive(Default)]
pub(super) struct TypeScriptProgramAuthorityLane {
    root: Option<PathBuf>,
    authorities: BTreeMap<TypeScriptProgramKey, TszProjectAuthority>,
    work: TypeScriptProgramWork,
}

impl TypeScriptProgramAuthorityLane {
    /// Retain at most the current admitted forest. Exact root equality is
    /// deliberately conservative; no filesystem canonicalization or ambient
    /// access can widen reuse to another package.
    pub(super) fn prepare(
        &mut self,
        root: &Path,
        keys: &[TypeScriptProgramKey],
    ) -> Result<(), TypeScriptProgramLaneError> {
        let retained = keys.iter().collect::<BTreeSet<_>>();
        if retained.len() != keys.len() {
            // Two real programs cannot borrow the same mutable authority.
            self.authorities.clear();
            return Err(TypeScriptProgramLaneError::DuplicateProgramIdentity);
        }
        if self.root.as_deref() != Some(root) {
            self.authorities.clear();
            self.root = Some(root.to_path_buf());
        }
        self.authorities.retain(|key, _| retained.contains(key));
        Ok(())
    }

    pub(super) fn authority(&mut self, key: TypeScriptProgramKey) -> &mut TszProjectAuthority {
        self.authorities.entry(key).or_default()
    }

    pub(super) fn project(&self, key: &TypeScriptProgramKey) -> Option<&TszProject> {
        self.authorities
            .get(key)
            .and_then(TszProjectAuthority::project)
    }

    pub(super) fn record(&mut self, report: TszUpdateReport) {
        self.work.parsed_and_bound = self
            .work
            .parsed_and_bound
            .saturating_add(report.parsed_and_bound as u64);
        self.work.reused_binds = self
            .work
            .reused_binds
            .saturating_add(report.reused_binds as u64);
        if report.reused_project_result {
            self.work.reused_checked_programs = self.work.reused_checked_programs.saturating_add(1);
        } else {
            self.work.checked_programs = self.work.checked_programs.saturating_add(1);
        }
    }

    #[cfg(test)]
    pub(super) fn work(&self) -> TypeScriptProgramWork {
        self.work
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_frontend_typescript::{
        TszCheckerOptions, TszEnvironmentFingerprint, TszFileInput, TszProjectOptions,
        TszProjectSemanticOptions,
    };

    fn recipe() -> TszProjectOptions {
        TszProjectOptions {
            checker: TszCheckerOptions {
                no_lib: true,
                ..TszCheckerOptions::default()
            },
            semantic_options: TszProjectSemanticOptions::declaration_scoped(),
            program_diagnostics: Vec::new(),
            module_resolutions: Vec::new(),
            file_formats: Vec::new(),
            environment: TszEnvironmentFingerprint::from_sha256([0x31; 32]),
        }
    }

    fn sources() -> Vec<TszFileInput> {
        vec![TszFileInput {
            path: "src/main.ts".into(),
            source: "export const value: number = 1;".into(),
        }]
    }

    #[test]
    fn lane_reuses_only_exact_root_and_retained_configured_programs() {
        let mut lane = TypeScriptProgramAuthorityLane::default();
        let a = Some("tsconfig.a.json".to_owned());
        let b = Some("tsconfig.b.json".to_owned());
        lane.prepare(Path::new("/a"), &[a.clone(), b.clone()])
            .unwrap();
        for key in [&a, &b] {
            let report = lane
                .authority(key.clone())
                .update(sources(), recipe(), &[])
                .unwrap();
            lane.record(report);
        }
        lane.prepare(Path::new("/a"), &[b.clone(), a.clone()])
            .unwrap();
        let report = lane
            .authority(a.clone())
            .update(sources(), recipe(), &[])
            .unwrap();
        assert!(report.reused_project_result);
        lane.record(report);
        assert_eq!(lane.work().checked_programs, 2);
        assert_eq!(lane.work().reused_checked_programs, 1);
        lane.prepare(Path::new("/a"), &[b.clone()]).unwrap();
        assert!(lane.project(&a).is_none());
        assert!(lane.project(&b).is_some());
        lane.prepare(Path::new("/b"), &[b.clone()]).unwrap();
        assert!(lane.project(&b).is_none());
        let report = lane.authority(b).update(sources(), recipe(), &[]).unwrap();
        assert!(!report.reused_project_result);
    }

    #[test]
    fn duplicate_nodes_cannot_alias_one_mutable_authority() {
        let mut lane = TypeScriptProgramAuthorityLane::default();
        lane.prepare(Path::new("/a"), &[None]).unwrap();
        lane.authority(None)
            .update(sources(), recipe(), &[])
            .unwrap();
        assert_eq!(
            lane.prepare(Path::new("/a"), &[None, None]),
            Err(TypeScriptProgramLaneError::DuplicateProgramIdentity)
        );
        assert!(lane.project(&None).is_none());
    }
}
