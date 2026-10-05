//! Authenticated store-head admission and upgrade classification, without a
//! listener, compiler, or native GUI. The existing embedded/desktop host tests
//! own the actual quarantine-and-reopen path.

use super::*;
use crate::builtin::BuiltinIntent;
use crate::builtin::profile::{
    BuiltinIntentOperation, BuiltinIntentSchema, BuiltinSemanticRelation, BuiltinSourceChange,
    BuiltinWorkspaceRelation, RetiredSourceProbe, prepare_transition_with_source_update,
};
use backend_engine::{
    ObjectKey, PreparedTransition, ProductSourceRecord, RelationAdmissionRegistry, SourceLanguage,
    TransactionId, TreeChange, TypedObject, WorkspaceModel, WorkspaceOwner, WorkspaceSnapshot,
};
use std::error::Error;
use std::path::Path;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn registry() -> TestResult<RelationAdmissionRegistry> {
    Ok(RelationAdmissionRegistry::new()
        .with_relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| std::io::Error::other(format!("source registry: {error:?}")))?
        .with_relation::<BuiltinSemanticRelation>()
        .map_err(|error| std::io::Error::other(format!("semantic registry: {error:?}")))?)
}

fn open_current(
    workspace: &Path,
) -> Result<WorkspaceOwner<BuiltinModel>, backend_engine::WorkspaceError> {
    let registry =
        registry().map_err(|error| backend_engine::WorkspaceError::Model(error.to_string()))?;
    let genesis = crate::builtin::genesis()
        .map_err(|error| backend_engine::WorkspaceError::Model(error.to_string()))?;
    WorkspaceOwner::open_with_registry(workspace, BuiltinModel, genesis, registry)
}

fn credential(workspace: &Path) -> TestResult<PathBuf> {
    let path = workspace.join("authority.secret");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    #[cfg(windows)]
    backend_platform::win32::security::restrict_to_current_user(&path)?;
    std::io::Write::write_all(&mut file, &[0x31; 32])?;
    file.sync_all()?;
    Ok(path)
}

/// Injects malformed membership/intent fixtures below the product model while
/// retaining real canonical relations, checked workspace envelopes, CAS, pack,
/// publication, and selected HEAD. This is never a serving or recovery model.
#[derive(Clone, Copy)]
enum IntentFault {
    None,
    TrailingBytes,
    DifferentRequest,
}

#[derive(Clone, Copy)]
struct StoredFixture {
    fault: IntentFault,
}

impl WorkspaceModel for StoredFixture {
    type Intent = BuiltinIntent;
    type Error = BuiltinModelError;

    fn request_id(&self, intent: &Self::Intent) -> [u8; 32] {
        BuiltinModel.request_id(intent)
    }

    fn prepare(
        &self,
        base: &WorkspaceSnapshot,
        intent: &Self::Intent,
        transaction: TransactionId,
    ) -> Result<PreparedTransition, Self::Error> {
        let relation = base
            .relation::<BuiltinWorkspaceRelation>()
            .map_err(|error| BuiltinModelError(error.to_string()))?;
        let changes = intent
            .changes()
            .iter()
            .map(|change| TreeChange {
                key: change.key,
                after: change.after.clone(),
            })
            .collect::<Vec<_>>();
        let update = relation
            .prepare_update(&changes)
            .map_err(|error| BuiltinModelError(error.to_string()))?;
        let transition =
            prepare_transition_with_source_update(base, intent, transaction, &relation, update)?;
        let bytes = match self.fault {
            IntentFault::None => return Ok(transition),
            IntentFault::TrailingBytes => {
                let mut bytes = intent.encode();
                bytes.push(0); // Content addressed, but invalid intent grammar.
                bytes
            }
            IntentFault::DifferentRequest => {
                // The same source delta, canonically encoded as a different
                // command, cannot carry the committed Index request identity.
                let mut retained = intent.clone();
                retained.operation = BuiltinIntentOperation::Add;
                retained.encode()
            }
        };
        let key = ObjectKey::<BuiltinIntentSchema>::from_value(&bytes);
        let object = TypedObject::from_value(&key, &bytes);
        let registry = registry().map_err(|error| BuiltinModelError(error.to_string()))?;
        transition
            .replace_object_family([object], &registry)
            .map_err(|error| BuiltinModelError(error.to_string()))
    }

    fn admit_persisted(
        &self,
        _persisted: &backend_engine::PersistedTransition,
    ) -> Result<PreparedTransition, Self::Error> {
        Err(BuiltinModelError(
            "stored fixture writer cannot recover".to_owned(),
        ))
    }
}

fn persist(workspace: &Path, intent: BuiltinIntent, fault: IntentFault) -> TestResult {
    let mut owner = WorkspaceOwner::open_with_registry(
        workspace,
        StoredFixture { fault },
        crate::builtin::genesis()?,
        registry()?,
    )?;
    let prepared = owner.prepare(owner.head().expectation(), intent)?;
    let durable = owner.durable(prepared)?;
    owner.publish(durable)?;
    Ok(())
}

fn file(project: [u8; 32], path: &str) -> TestResult<ProductSourceRecord> {
    Ok(ProductSourceRecord::file_within_row_capacity(
        project,
        path,
        SourceLanguage::Rust,
        [7; 32],
        [1; 32],
        Vec::new(),
    )?)
}

fn intent(
    label: &str,
    project: ProductSourceRecord,
    files: Vec<([u8; 32], ProductSourceRecord)>,
) -> TestResult<BuiltinIntent> {
    let package = backend_engine::package_key(label);
    let mut changes = vec![BuiltinSourceChange {
        key: package.to_bytes(),
        after: Some(project),
    }];
    changes.extend(files.into_iter().map(|(key, record)| BuiltinSourceChange {
        key,
        after: Some(record),
    }));
    Ok(BuiltinIntent::index_with_semantics(
        package,
        label,
        changes,
        Vec::new(),
    )?)
}

#[test]
fn honest_retired_writer_is_refused_by_current_model_and_certified_by_private_probe() -> TestResult
{
    let root = tempfile::TempDir::new()?;
    crate::test_support::make_private(root.path())?;
    let secret = credential(root.path())?;
    write_state_from_another_build(root.path(), &secret)?;
    let journal = std::fs::read(root.path().join("workspace.journal"))?;
    assert!(!journal.is_empty());
    let error = open_current(root.path())
        .err()
        .ok_or("current model admitted retired keys")?;
    assert!(
        matches!(&error, backend_engine::WorkspaceError::Model(_)),
        "{error:?}"
    );
    let evidence = probe_retired_layout(root.path())?;
    assert_eq!(evidence.files.get(), 1);
    let refusal = owner_open_refusal(root.path(), crate::LocaldError::Workspace(error));
    assert!(
        matches!(&refusal, ProcessError::StateFromAnotherBuild(_)),
        "{refusal:?}"
    );
    assert_eq!(
        std::fs::read(root.path().join("workspace.journal"))?,
        journal
    );
    assert_eq!(std::fs::read(secret)?, vec![0x31; 32]);
    assert!(
        !root.path().join("from-another-build").exists(),
        "classification itself moves nothing"
    );

    let probe = WorkspaceOwner::open_with_registry(
        root.path(),
        RetiredSourceProbe::new(root.path()),
        crate::builtin::genesis()?,
        registry()?,
    )?;
    let added = BuiltinIntent::add(
        backend_engine::package_key("/project/not-served"),
        "/project/not-served",
    )?;
    assert!(
        probe.prepare(probe.head().expectation(), added).is_err(),
        "the probe cannot publish"
    );
    // No additional store publication was made by any of these probes.
    assert_eq!(probe.head().sequence(), 1);
    Ok(())
}

#[test]
fn current_source_layout_is_never_classified_as_historical() -> TestResult {
    let root = tempfile::TempDir::new()?;
    crate::test_support::make_private(root.path())?;
    let label = "/project/current-layout";
    let project = backend_engine::package_key(label).to_bytes();
    let key = product_source_file_key(project, "src/lib.rs");
    persist(
        root.path(),
        intent(
            label,
            ProductSourceRecord::project(label, [9; 32], vec![key])?,
            vec![(key, file(project, "src/lib.rs")?)],
        )?,
        IntentFault::None,
    )?;
    let journal = std::fs::read(root.path().join("workspace.journal"))?;
    let owner = open_current(root.path())?;
    assert_eq!(owner.head().sequence(), 1);
    drop(owner);
    assert!(probe_retired_layout(root.path()).is_err());
    assert_eq!(
        std::fs::read(root.path().join("workspace.journal"))?,
        journal
    );
    Ok(())
}

#[test]
fn mixed_foreign_damaged_and_missing_source_membership_remain_profile_refusals() -> TestResult {
    let label = "/project/not-an-upgrade";
    let project = backend_engine::package_key(label).to_bytes();
    let first = legacy_product_source_file_key(project, "src/a.rs");
    let current = product_source_file_key(project, "src/b.rs");
    let foreign = backend_engine::package_key("/project/foreign-owner").to_bytes();
    let mut mixed = vec![first, current];
    mixed.sort_unstable();
    let mut page_keys = (0..2048)
        .map(|at| legacy_product_source_file_key(project, &format!("src/page-{at}.rs")))
        .collect::<Vec<_>>();
    page_keys.sort_unstable();
    let paged =
        ProductSourceRecord::project_with_membership_pages(label, [9; 32], page_keys, None)?;
    assert!(
        !paged.membership_pages().is_empty(),
        "the missing-page fixture must actually be paged"
    );
    let cases = [
        (
            "mixed",
            ProductSourceRecord::project(label, [9; 32], mixed)?,
            vec![
                (first, file(project, "src/a.rs")?),
                (current, file(project, "src/b.rs")?),
            ],
        ),
        (
            "foreign",
            ProductSourceRecord::project(label, [9; 32], vec![first])?,
            vec![(first, file(foreign, "src/a.rs")?)],
        ),
        (
            "damaged-key",
            ProductSourceRecord::project(label, [9; 32], vec![[0xee; 32]])?,
            vec![([0xee; 32], file(project, "src/a.rs")?)],
        ),
        (
            "missing-file",
            ProductSourceRecord::project(label, [9; 32], vec![first])?,
            vec![],
        ),
        (
            "orphan-file",
            ProductSourceRecord::project(label, [9; 32], vec![])?,
            vec![(first, file(project, "src/a.rs")?)],
        ),
        ("missing-page", paged.project_record().clone(), vec![]),
    ];
    for (case, record, files) in cases {
        let root = tempfile::TempDir::new()?;
        crate::test_support::make_private(root.path())?;
        persist(
            root.path(),
            intent(label, record, files)?,
            IntentFault::None,
        )?;
        let journal = std::fs::read(root.path().join("workspace.journal"))?;
        let error = open_current(root.path())
            .err()
            .ok_or("current model admitted malformed membership")?;
        let refusal = owner_open_refusal(root.path(), crate::LocaldError::Workspace(error));
        assert!(
            matches!(&refusal, ProcessError::Profile(_)),
            "{case}: {refusal:?}"
        );
        assert!(
            probe_retired_layout(root.path()).is_err(),
            "{case}: retired probe admitted corruption"
        );
        assert_eq!(
            std::fs::read(root.path().join("workspace.journal"))?,
            journal,
            "{case}"
        );
        assert!(
            !root.path().join("from-another-build").exists(),
            "{case}: refused state was moved"
        );
    }
    Ok(())
}

#[test]
fn malformed_or_request_mismatched_persisted_intents_cannot_trigger_quarantine() -> TestResult {
    for fault in [IntentFault::TrailingBytes, IntentFault::DifferentRequest] {
        let root = tempfile::TempDir::new()?;
        crate::test_support::make_private(root.path())?;
        let label = "/project/malformed-transition";
        let project = backend_engine::package_key(label).to_bytes();
        let key = legacy_product_source_file_key(project, "src/lib.rs");
        persist(
            root.path(),
            intent(
                label,
                ProductSourceRecord::project(label, [9; 32], vec![key])?,
                vec![(key, file(project, "src/lib.rs")?)],
            )?,
            fault,
        )?;
        let journal = std::fs::read(root.path().join("workspace.journal"))?;
        let error = open_current(root.path())
            .err()
            .ok_or("current model admitted malformed persisted intent")?;
        let refusal = owner_open_refusal(root.path(), crate::LocaldError::Workspace(error));
        assert!(matches!(&refusal, ProcessError::Profile(_)), "{refusal:?}");
        let probe_error = probe_retired_layout(root.path())
            .err()
            .ok_or("retired probe admitted a malformed or mismatched intent")?;
        let expected = match fault {
            IntentFault::TrailingBytes => "trailing product source intent bytes",
            IntentFault::DifferentRequest => {
                "retired persisted intent does not match its request identity"
            }
            IntentFault::None => "fixture must inject an intent fault",
        };
        assert!(probe_error.contains(expected), "{probe_error}");
        assert_eq!(
            std::fs::read(root.path().join("workspace.journal"))?,
            journal
        );
        assert!(!root.path().join("from-another-build").exists());
    }
    Ok(())
}

#[test]
fn a_torn_diagnostic_tail_is_preserved_and_never_triggers_upgrade_quarantine() -> TestResult {
    let root = tempfile::TempDir::new()?;
    crate::test_support::make_private(root.path())?;
    let secret = credential(root.path())?;
    write_state_from_another_build(root.path(), &secret)?;
    let path = root.path().join("workspace.journal");
    let mut journal = std::fs::read(&path)?;
    journal.push(0xff);
    std::fs::write(&path, &journal)?;
    let error = open_current(root.path())
        .err()
        .ok_or("current model admitted retired keys")?;
    let refusal = owner_open_refusal(root.path(), crate::LocaldError::Workspace(error));
    assert!(matches!(&refusal, ProcessError::Profile(_)), "{refusal:?}");
    assert_eq!(
        std::fs::read(path)?,
        journal,
        "classification must not repair a torn tail"
    );
    assert!(!root.path().join("from-another-build").exists());
    Ok(())
}

#[test]
fn the_probe_checks_unrelated_frontiers_in_both_authenticated_roots() -> TestResult {
    for first_retired in [false, true] {
        let root = tempfile::TempDir::new()?;
        crate::test_support::make_private(root.path())?;
        let mut owner = WorkspaceOwner::open_with_registry(
            root.path(),
            StoredFixture {
                fault: IntentFault::None,
            },
            crate::builtin::genesis()?,
            registry()?,
        )?;
        for (label, retired) in [
            ("/project/first-frontier", first_retired),
            ("/project/selected-last", true),
        ] {
            let project = backend_engine::package_key(label).to_bytes();
            let key = if retired {
                legacy_product_source_file_key(project, "src/lib.rs")
            } else {
                product_source_file_key(project, "src/lib.rs")
            };
            let intent = intent(
                label,
                ProductSourceRecord::project(label, [9; 32], vec![key])?,
                vec![(key, file(project, "src/lib.rs")?)],
            )?;
            let prepared = owner.prepare(owner.head().expectation(), intent)?;
            let durable = owner.durable(prepared)?;
            owner.publish(durable)?;
        }
        assert_eq!(owner.head().sequence(), 2);
        drop(owner);
        let journal = std::fs::read(root.path().join("workspace.journal"))?;
        let error = open_current(root.path())
            .err()
            .ok_or("current model admitted the final retired file")?;
        let refusal = owner_open_refusal(root.path(), crate::LocaldError::Workspace(error));
        if first_retired {
            assert!(
                matches!(&refusal, ProcessError::StateFromAnotherBuild(_)),
                "{refusal:?}"
            );
            assert_eq!(probe_retired_layout(root.path())?.files.get(), 2);
        } else {
            assert!(
                matches!(&refusal, ProcessError::Profile(_)),
                "a retired final delta must not quarantine a current unrelated project: {refusal:?}"
            );
            assert!(probe_retired_layout(root.path()).is_err());
        }
        assert_eq!(
            std::fs::read(root.path().join("workspace.journal"))?,
            journal
        );
        assert!(!root.path().join("from-another-build").exists());
    }
    Ok(())
}
