//! What a host that embeds the owner adds to its composition: the compiler
//! paths it supplies itself, and the recognition of a workspace another build
//! of the product wrote.

use super::view_journal::ViewJournal;
use super::{
    BuiltinAuthorityVerifier, BuiltinModel, BuiltinModelError, IndexedSources, ProcessError,
    read_indexed_sources,
};
use backend_engine::application::{
    ClosedLocalHostEnvironmentSnapshot, LocalHostEnvironment, LocalHostVariable,
    ProcessHostEnvironment,
};
use backend_engine::{legacy_product_source_file_key, product_source_file_key};
use std::ffi::OsString;
use std::path::PathBuf;

/// A refused store is historical only after a private, non-serving owner
/// re-admits its complete selected transition under the exact retired layout.
/// Generic model errors, current keys, and damaged retired frontiers never
/// manufacture this evidence.
struct RetiredLayoutEvidence {
    files: std::num::NonZeroUsize,
}

fn probe_retired_layout(workspace: &std::path::Path) -> Result<RetiredLayoutEvidence, String> {
    use super::profile::{BuiltinSemanticRelation, BuiltinWorkspaceRelation, RetiredSourceProbe};
    use backend_engine::{RelationAdmissionRegistry, WorkspaceOwner};

    let registry = RelationAdmissionRegistry::new()
        .with_relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| format!("register source relation: {error:?}"))?
        .with_relation::<BuiltinSemanticRelation>()
        .map_err(|error| format!("register semantic relation: {error:?}"))?
        .with_relation::<backend_engine::builtin::ProductSemanticCaptureRelation>()
        .map_err(|error| format!("register semantic capture relation: {error:?}"))?;
    let owner = WorkspaceOwner::open_with_registry(
        workspace,
        RetiredSourceProbe::new(workspace),
        super::genesis().map_err(|error| error.to_string())?,
        registry,
    )
    .map_err(|error| error.to_string())?;
    if owner.head().sequence() == 0 {
        return Err("no selected historical store head".to_owned());
    }
    let relation = owner
        .snapshot()
        .relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| error.to_string())?;
    let sources =
        super::read_indexed_relation(&relation, super::profile::SourceFileKeyLayout::Retired)
            .map_err(|error| error.to_string())?;
    let files = std::num::NonZeroUsize::new(sources.files.len())
        .ok_or_else(|| "no retired source files".to_owned())?;
    Ok(RetiredLayoutEvidence { files })
}

/// The ordinary model always refuses retired keys. This second admission is
/// only an upgrade discriminator, under the actual workspace lease and the
/// engine's authenticated HEAD/closure/pack recovery. No listener or compiler
/// receives the historical owner, and it cannot plan a mutation.
pub(super) fn owner_open_refusal(
    workspace: &std::path::Path,
    error: crate::LocaldError,
) -> ProcessError {
    if matches!(
        &error,
        crate::LocaldError::Workspace(backend_engine::WorkspaceError::Model(_))
    ) && let Ok(evidence) = probe_retired_layout(workspace)
    {
        return ProcessError::StateFromAnotherBuild(format!(
            "{} indexed source files are keyed by the source-file key layout an earlier build wrote ({error})",
            evidence.files
        ));
    }
    ProcessError::Profile(error.to_string())
}

/// The environment [`backend_engine::application::LocalCompilerHost::production_at`]
/// reads, with the paths an embedding host supplied placed ahead of the
/// process's own. The compiler root is the workspace's, as there.
pub(super) struct EmbeddedCompilerEnvironment {
    pub(super) data_root: PathBuf,
    pub(super) compiler_environment: Option<ClosedLocalHostEnvironmentSnapshot>,
    /// Captured once while locald composes its compiler owner. The selected runtime is pinned
    /// before the daemon starts serving requests; subsequent clients cannot refresh this PATH.
    pub(super) search_path: Option<OsString>,
}

impl LocalHostEnvironment for EmbeddedCompilerEnvironment {
    fn value(&self, variable: LocalHostVariable) -> Option<OsString> {
        self.value_with(variable, |variable| ProcessHostEnvironment.value(variable))
    }

    fn search_path(&self) -> Option<OsString> {
        self.search_path.clone()
    }
}

impl EmbeddedCompilerEnvironment {
    fn value_with(
        &self,
        variable: LocalHostVariable,
        ambient: impl FnOnce(LocalHostVariable) -> Option<OsString>,
    ) -> Option<OsString> {
        if variable == LocalHostVariable::NudoxDataRoot {
            return Some(self.data_root.clone().into_os_string());
        }
        match &self.compiler_environment {
            Some(snapshot) => snapshot
                .path(variable)
                .map(|path| path.as_os_str().to_os_string()),
            None => ambient(variable),
        }
    }
}

/// How many indexed source files are keyed by the layout a build before the
/// project-affine one wrote (`legacy_product_source_file_key`).
///
/// Only a key that is the retired derivation of the file's own project and
/// path counts. A key that is neither that nor the current one is damage, and
/// stays damage.
fn files_keyed_by_the_retired_layout(sources: &IndexedSources) -> usize {
    sources
        .files
        .iter()
        .filter(|(key, record)| {
            record.file_fields().is_some_and(|file| {
                *key != product_source_file_key(file.project, file.path)
                    && *key == legacy_product_source_file_key(file.project, file.path)
            })
        })
        .count()
}

/// Says why the owner would not recover its view journal: another build wrote
/// it in a format this one does not speak, or it is damaged.
///
/// The journal only ever holds a derived view, but recovery refuses whole on a
/// version it does not read. The journal's own version numbers decide
/// (`ViewJournal::written_by_another_build`), after the refusal and never from
/// its words.
pub(super) fn journal_refusal(journal: &ViewJournal, error: String) -> ProcessError {
    match journal.written_by_another_build() {
        Some(evidence) => ProcessError::StateFromAnotherBuild(format!("{evidence} ({error})")),
        None => ProcessError::Profile(error),
    }
}

/// Says why the owner would not build its view of the workspace: this build
/// cannot read state another build wrote, or something is wrong.
///
/// The view is built from the indexed sources, and the check that fails on an
/// old workspace ("outside its project's canonical frontier") fails the same
/// way on a corrupt one. The two are told apart by evidence, after the fact and
/// only for a workspace that has already failed: the old workspace's file keys
/// are exactly the retired layout's. `context` is the prefix the refusal has
/// when the workspace is not from another build.
pub(super) fn view_refusal(
    daemon: &crate::Locald<BuiltinModel, super::BuiltinValidator, BuiltinAuthorityVerifier>,
    error: &BuiltinModelError,
    context: &str,
) -> ProcessError {
    let snapshot = daemon.engine().daemon().owner().snapshot();
    match read_indexed_sources(&snapshot).map(|sources| files_keyed_by_the_retired_layout(&sources))
    {
        Ok(retired) if retired > 0 => ProcessError::StateFromAnotherBuild(format!(
            "{retired} indexed source files are keyed by the source-file key layout an earlier build wrote ({error})"
        )),
        _ => ProcessError::Profile(format!("{context}{error}")),
    }
}

/// Writes a workspace as a build before the project-affine source-file key
/// layout left it: one project holding one Rust file, the file keyed by
/// [`legacy_product_source_file_key`].
///
/// The records are the ones such a build wrote (they decode, and the record
/// version did not change); only the file's key differs from the current
/// layout. `authority_secret` is the workspace's 32-byte credential, which must
/// already exist. Nothing else is created, so the workspace is what a host finds
/// after an upgrade with nothing yet indexed under the new build.
///
/// # Errors
///
/// Returns the first refusal of composing or committing to the workspace.
#[cfg(any(test, feature = "test-support"))]
pub fn write_state_from_another_build(
    workspace: &std::path::Path,
    authority_secret: &std::path::Path,
) -> Result<(), String> {
    use super::BuiltinIntent;
    use super::profile::{BuiltinSemanticRelation, BuiltinSourceChange, BuiltinWorkspaceRelation};
    use backend_engine::{ProductSourceRecord, RelationAdmissionRegistry, WorkspaceOwner};
    use backend_engine::{SourceLanguage, package_key};

    let _credential = backend_engine::read_authority_secret(authority_secret)
        .map_err(|error| error.to_string())?;
    let registry = RelationAdmissionRegistry::new()
        .with_relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| format!("register source relation: {error:?}"))?
        .with_relation::<BuiltinSemanticRelation>()
        .map_err(|error| format!("register semantic relation: {error:?}"))?
        .with_relation::<backend_engine::builtin::ProductSemanticCaptureRelation>()
        .map_err(|error| format!("register semantic capture relation: {error:?}"))?;
    let mut owner = WorkspaceOwner::open_with_registry(
        workspace,
        RetiredFixtureWriter,
        super::genesis().map_err(|error| error.to_string())?,
        registry,
    )
    .map_err(|error| error.to_string())?;

    let label = "/project/written-by-another-build";
    let package = package_key(label);
    let project_key = package.to_bytes();
    let path = "src/lib.rs";
    let file_key = legacy_product_source_file_key(project_key, path);
    let file = ProductSourceRecord::file_within_row_capacity(
        project_key,
        path,
        SourceLanguage::Rust,
        [7; 32],
        [1; 32],
        Vec::new(),
    )?;
    let project = ProductSourceRecord::project(label, [9; 32], vec![file_key])?;
    let intent = BuiltinIntent::index_with_semantics(
        package,
        label,
        vec![
            BuiltinSourceChange {
                key: project_key,
                after: Some(project),
            },
            BuiltinSourceChange {
                key: file_key,
                after: Some(file),
            },
        ],
        Vec::new(),
    )
    .map_err(|error| error.to_string())?;
    let prepared = owner
        .prepare(owner.head().expectation(), intent)
        .map_err(|error| error.to_string())?;
    let durable = owner.durable(prepared).map_err(|error| error.to_string())?;
    owner.publish(durable).map_err(|error| error.to_string())?;
    Ok(())
}

/// The previous build's planner is reachable only by fixture construction.
/// It shares the checked transition/closure wire path with the current model;
/// its complete membership proof uses the retired key formula.
#[cfg(any(test, feature = "test-support"))]
struct RetiredFixtureWriter;

#[cfg(any(test, feature = "test-support"))]
impl backend_engine::WorkspaceModel for RetiredFixtureWriter {
    type Intent = super::BuiltinIntent;
    type Error = BuiltinModelError;

    fn request_id(&self, intent: &Self::Intent) -> [u8; 32] {
        backend_engine::WorkspaceModel::request_id(&BuiltinModel, intent)
    }

    fn prepare(
        &self,
        base: &backend_engine::WorkspaceSnapshot,
        intent: &Self::Intent,
        transaction: backend_engine::TransactionId,
    ) -> Result<backend_engine::PreparedTransition, Self::Error> {
        super::profile::prepare_retired_fixture(base, intent, transaction)
    }

    fn admit_persisted(
        &self,
        _persisted: &backend_engine::PersistedTransition,
    ) -> Result<backend_engine::PreparedTransition, Self::Error> {
        Err(BuiltinModelError(
            "retired fixture writer requires an empty workspace".to_owned(),
        ))
    }
}

#[cfg(test)]
#[path = "embedded_host_retired_tests.rs"]
mod retired_tests;

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use backend_engine::{ProductSourceRecord, SourceLanguage};
    use std::collections::BTreeMap;

    fn file_record(project: [u8; 32], path: &str) -> ProductSourceRecord {
        ProductSourceRecord::file_within_row_capacity(
            project,
            path.to_owned(),
            SourceLanguage::Rust,
            [7; 32],
            [1; 32],
            Vec::new(),
        )
        .expect("file record")
    }

    fn sources(files: Vec<([u8; 32], ProductSourceRecord)>) -> IndexedSources {
        IndexedSources {
            projects: BTreeMap::new(),
            files,
            cargo_aliases: BTreeMap::new(),
            source_snapshot: None,
        }
    }

    #[test]
    fn only_keys_of_the_retired_layout_count_as_state_from_another_build() {
        let project = [0x21; 32];
        let current = ("src/a.rs", product_source_file_key(project, "src/a.rs"));
        let retired = (
            "src/b.rs",
            legacy_product_source_file_key(project, "src/b.rs"),
        );
        // Neither layout's key for this path: damage, not another build.
        let damaged = ("src/c.rs", [0xee; 32]);
        let listed = |(path, key): (&str, [u8; 32])| (key, file_record(project, path));

        assert_eq!(
            files_keyed_by_the_retired_layout(&sources(vec![listed(current), listed(damaged)])),
            0,
            "a current workspace with one damaged key is not from another build"
        );
        assert_eq!(
            files_keyed_by_the_retired_layout(&sources(vec![
                listed(current),
                listed(retired),
                listed(damaged),
            ])),
            1,
            "exactly the retired-layout key is counted"
        );
    }

    #[test]
    fn closed_environment_seals_missing_roles_and_keeps_the_workspace_data_root() {
        use backend_engine::application::ClosedLocalHostEnvironmentSnapshot;

        let cargo = std::env::temp_dir().join("tc").join("bin").join("cargo");
        let environment = EmbeddedCompilerEnvironment {
            data_root: PathBuf::from("/workspace/compiler"),
            compiler_environment: Some(
                ClosedLocalHostEnvironmentSnapshot::from_paths([(
                    LocalHostVariable::NudoxCargo,
                    cargo.clone(),
                )])
                .expect("valid closed compiler environment"),
            ),
            search_path: Some(OsString::from("/captured/compiler/bin")),
        };

        assert_eq!(
            environment.value(LocalHostVariable::NudoxCargo),
            Some(cargo.into_os_string()),
        );
        assert_eq!(
            environment.value(LocalHostVariable::NudoxDataRoot),
            Some(OsString::from("/workspace/compiler")),
            "the compiler root stays the workspace's"
        );
        assert_eq!(
            environment.search_path(),
            Some(OsString::from("/captured/compiler/bin")),
            "the service receives the process search path captured at startup for TypeScript host discovery",
        );
        let mut consulted_ambient = false;
        assert_eq!(
            environment.value_with(LocalHostVariable::Home, |_| {
                consulted_ambient = true;
                Some(OsString::from("/hostile/ambient/home"))
            }),
            None,
            "an omitted role in a supplied snapshot is absent"
        );
        assert!(
            !consulted_ambient,
            "closed mode must not consult ambient values"
        );
    }

    #[test]
    fn absent_snapshot_retains_the_standalone_ambient_mode() {
        let environment = EmbeddedCompilerEnvironment {
            data_root: PathBuf::from("/workspace/compiler"),
            compiler_environment: None,
            search_path: None,
        };
        assert_eq!(
            environment.value_with(LocalHostVariable::Home, |_| {
                Some(OsString::from("/ambient/operator/home"))
            }),
            Some(OsString::from("/ambient/operator/home")),
        );
    }
}
