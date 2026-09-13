use super::*;
use crate::contract::{AuthorityAdmissionError, AuthorityRegistry};
use std::{
    error::Error, fmt::Write as _, fs, io::Write as _, path::PathBuf, process::Command, sync::Arc,
    thread, time::Duration,
};

#[test]
fn declaration_metadata_is_typed_and_canonical() -> Result<(), String> {
    assert_eq!(
        DeclarationKind::from_name("namespace"),
        DeclarationKind::Module
    );
    assert_eq!(
        DeclarationKind::from_name("type_alias"),
        DeclarationKind::Type
    );
    assert_eq!(
        syntax::declaration_kind(SourceLanguage::Rust, "class", "struct_item"),
        DeclarationKind::Struct
    );
    assert_eq!(
        syntax::declaration_kind(SourceLanguage::Rust, "class", "enum_item"),
        DeclarationKind::Enum
    );
    assert_eq!(
        syntax::declaration_kind(SourceLanguage::Rust, "interface", "trait_item"),
        DeclarationKind::Trait
    );
    assert_eq!(
        syntax::declaration_kind(SourceLanguage::Java, "interface", "interface_declaration"),
        DeclarationKind::Interface
    );
    assert_eq!(
        syntax::declaration_kind(SourceLanguage::Clang, "class", "struct_specifier"),
        DeclarationKind::Struct
    );
    assert_eq!(
        DeclarationKind::from_wire_tag(DeclarationKind::Method.wire_tag()),
        Some(DeclarationKind::Method)
    );
    assert_eq!(DeclarationKind::from_wire_tag(0), None);

    let declaration = SourceDeclaration::at_path(
        "src/lib.rs",
        "answer",
        "function",
        42,
        "fn answer() -> u32",
        "the answer",
    )?;
    assert_eq!(declaration.kind(), DeclarationKind::Function);
    assert_eq!(declaration.location().path(), "src/lib.rs");
    assert_eq!(declaration.location().start_line(), 42);
    assert_eq!(declaration.line(), 42);
    Ok(())
}

#[test]
fn source_excerpt_is_bounded_before_ownership_and_preserves_utf8_extent() {
    let source = format!("{}é", "x".repeat(SourceExcerpt::MAX_BYTES));
    let excerpt = SourceExcerpt::capture_bounded(&source);
    assert_eq!(excerpt.text().map(str::len), Some(SourceExcerpt::MAX_BYTES));
    assert_eq!(excerpt.extent(), Some(SourceExcerptExtent::Truncated));
    let oversized = "x".repeat(SourceExcerpt::MAX_BYTES + 1);
    assert!(SourceExcerpt::captured(&oversized, SourceExcerptExtent::Complete).is_err());
}

struct MockAuthority {
    revision: u64,
    unavailable: bool,
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "test call sites use ? uniformly with fallible manifest fixtures"
)]
fn test_registry(
    manifest: &InputManifest,
    revision: u64,
) -> Result<AuthorityRegistry, Box<dyn Error>> {
    let authority = MockAuthority::identity_value();
    let snapshot = DiscoverySnapshot::new(manifest.clone(), revision);
    Ok(AuthorityRegistry::for_test(authority, &snapshot))
}

fn complete_test_snapshot(
    manifest: &InputManifest,
    revision: u64,
    records: Vec<FactRecord<FactKeySchema, FactValueSchema>>,
) -> Result<FactSnapshot<FactKeySchema, FactValueSchema>, Box<dyn Error>> {
    let registry = test_registry(manifest, revision)?;
    let (capability, records) = registry
        .complete_records(records)
        .map_err(|error| error.to_string())?;
    FactSnapshot::from_complete_capability(capability, records)
        .map_err(|error| error.to_string().into())
}

fn forged_complete(manifest: &InputManifest) -> Result<CoverageWitness, Box<dyn Error>> {
    let registry = test_registry(manifest, 0)?;
    Ok(CoverageWitness::Complete(
        registry.admit_complete_coverage()?,
    ))
}

impl MockAuthority {
    fn identity_value() -> AuthorityIdentity {
        AuthorityIdentity {
            producer: typed_of::<ProducerSchema>(b"mock"),
            toolchain: typed_of::<ToolchainSchema>(b"tool"),
            contract: typed_of::<ContractSchema>(b"contract"),
        }
    }
}

impl Authority for MockAuthority {
    fn identity(&self) -> AuthorityIdentity {
        Self::identity_value()
    }

    fn discover(&self) -> Result<DiscoverySnapshot, AuthorityError> {
        let source = Input::new(InputKind::Source, "src/main", b"x")
            .map_err(|error| AuthorityError::Discovery(error.to_string()))?;
        let negative = Input::absent(InputKind::NegativeDependency, "dep/optional")
            .map_err(|error| AuthorityError::Discovery(error.to_string()))?;
        let manifest = InputManifest::new(vec![negative, source])
            .map_err(|error| AuthorityError::Discovery(error.to_string()))?;
        Ok(DiscoverySnapshot::new(manifest, self.revision))
    }

    fn extract(
        &self,
        snapshot: &DiscoverySnapshot,
        _key: SessionKey,
    ) -> Result<Extraction, AuthorityError> {
        if self.unavailable {
            return Extraction::new(
                CoverageWitness::Unavailable(UntrustedCoverageScope::new(0)),
                Vec::new(),
            )
            .map_err(|error| AuthorityError::Extraction(error.to_string()));
        }
        let registry = test_registry(snapshot.manifest(), snapshot.sequence())
            .map_err(|error| AuthorityError::Extraction(error.to_string()))?;
        let records = vec![FactRecord::<FactKeySchema, FactValueSchema>::new(
            FactKind::Declaration,
            snapshot.sequence().to_string(),
            b"fact".to_vec(),
        )];
        let (capability, records) = registry
            .complete_records(records)
            .map_err(|error| AuthorityError::Extraction(error.to_string()))?;
        Extraction::from_complete_capability(capability, records)
            .map_err(|error| AuthorityError::Extraction(error.to_string()))
    }
}

fn session_key(authority: &MockAuthority, snapshot: &DiscoverySnapshot) -> SessionKey {
    SessionKey::new(
        authority.identity(),
        snapshot.manifest(),
        typed_of::<ProfileSchema>(b"profile"),
        typed_of::<FlowSchema>(b"flow"),
        typed_of::<SemanticBasisSchema>(b"semantic"),
    )
}

fn limits(
    stdout: usize,
    stderr: usize,
    wall_time: Duration,
    workspace_bytes: usize,
) -> Result<ProcessLimits, ProcessError> {
    ProcessLimits::new(stdout, stderr, wall_time, workspace_bytes)
}

#[test]
fn manifests_are_deterministic_and_include_negative_inputs() -> Result<(), Box<dyn Error>> {
    let authority = MockAuthority {
        revision: 1,
        unavailable: false,
    };
    let first = authority.discover()?;
    let second = authority.discover()?;
    assert_eq!(first.manifest().digest(), second.manifest().digest());
    assert!(
        first
            .manifest()
            .get(InputKind::NegativeDependency, "dep/optional")
            .is_some_and(|input| !input.is_present())
    );
    Ok(())
}

#[test]
fn deltas_distinguish_modified_and_added() -> Result<(), Box<dyn Error>> {
    let authority = MockAuthority {
        revision: 1,
        unavailable: false,
    };
    let old = authority.discover()?;
    let new = InputManifest::new(vec![
        Input::new(InputKind::Source, "src/main", b"y")?,
        Input::new(InputKind::Generated, "build/out", b"z")?,
    ])?;
    let delta = DiscoveryDelta::between(old.manifest(), &new);
    assert_eq!(delta.changes().len(), 3);
    assert!(
        delta
            .changes()
            .iter()
            .any(|change| matches!(change, InputChange::Modified { .. }))
    );
    Ok(())
}

#[test]
fn session_key_changes_with_authority_and_manifest() -> Result<(), Box<dyn Error>> {
    let authority = MockAuthority {
        revision: 1,
        unavailable: false,
    };
    let first = authority.discover()?;
    let key = session_key(&authority, &first);
    let changed = InputManifest::new(vec![
        Input::new(InputKind::Source, "src/main", b"changed")?,
        Input::absent(InputKind::NegativeDependency, "dep/optional")?,
    ])?;
    let changed_key = SessionKey::new(
        authority.identity(),
        &changed,
        typed_of::<ProfileSchema>(b"profile"),
        typed_of::<FlowSchema>(b"flow"),
        typed_of::<SemanticBasisSchema>(b"semantic"),
    );
    assert_ne!(key, changed_key);
    assert!(key.matches(&authority.identity(), first.manifest()));
    Ok(())
}

#[test]
fn partial_and_unavailable_are_explicit() -> Result<(), Box<dyn Error>> {
    let authority = MockAuthority {
        revision: 1,
        unavailable: true,
    };
    let snapshot = authority.discover()?;
    let extraction = authority.extract(&snapshot, session_key(&authority, &snapshot))?;
    assert!(!extraction.coverage().state().is_complete());
    assert_eq!(extraction.authority_root(), None);
    Ok(())
}

#[test]
fn complete_extraction_requires_a_matching_bound_scope() -> Result<(), Box<dyn Error>> {
    let authority = MockAuthority {
        revision: 1,
        unavailable: false,
    };
    let snapshot = authority.discover()?;
    assert!(!snapshot.complete_coverage().state().is_complete());
    let wrong_manifest =
        InputManifest::new(vec![Input::new(InputKind::Source, "different", b"x")?])?;
    let result = Extraction::bound(
        authority.identity(),
        &wrong_manifest,
        forged_complete(&wrong_manifest)?,
        Vec::new(),
    );
    assert_eq!(result, Err(ExtractionError::UnboundComplete));
    Ok(())
}

#[cfg(unix)]
fn native_test_registry(
    manifest: &InputManifest,
    revision: u64,
) -> Result<AuthorityRegistry, Box<dyn Error>> {
    let authority = MockAuthority::identity_value();
    let snapshot = DiscoverySnapshot::new(manifest.clone(), revision);
    let key = session_key(
        &MockAuthority {
            revision,
            unavailable: false,
        },
        &snapshot,
    );
    let command = SupervisedCommand::for_authority(
        std::env::current_exe()?,
        Vec::new(),
        ProcessEnvironment::new(Vec::new())?,
        std::env::current_dir()?,
        ProcessStdin::null(),
        authority.toolchain,
        Some(key),
        ProtocolDescriptor::cold(),
        limits(64, 64, Duration::from_secs(1), 128)?,
    )?;
    AuthorityRegistry::for_native(authority, &snapshot, &command)
        .map_err(|error| error.to_string().into())
}

#[cfg(unix)]
#[test]
fn complete_authority_requires_closed_manifest_and_typed_facts() -> Result<(), Box<dyn Error>> {
    let open_manifest =
        InputManifest::new(vec![Input::new(InputKind::Source, "src/main", b"source")?])?;
    let open_snapshot = DiscoverySnapshot::new(open_manifest.clone(), 1);
    let open_authority = MockAuthority::identity_value();
    let open_key = session_key(
        &MockAuthority {
            revision: 1,
            unavailable: false,
        },
        &open_snapshot,
    );
    let open_command = SupervisedCommand::for_authority(
        std::env::current_exe()?,
        Vec::new(),
        ProcessEnvironment::new(Vec::new())?,
        std::env::current_dir()?,
        ProcessStdin::null(),
        open_authority.toolchain,
        Some(open_key),
        ProtocolDescriptor::cold(),
        limits(64, 64, Duration::from_secs(1), 128)?,
    )?;
    assert_eq!(
        AuthorityRegistry::for_native(open_authority, &open_snapshot, &open_command),
        Err(AuthorityAdmissionError::OpenManifest)
    );

    let closed_manifest = InputManifest::new(vec![
        Input::new(InputKind::Source, "src/main", b"source")?,
        Input::absent(InputKind::NegativeDependency, "dep/optional")?,
    ])?;
    let registry = native_test_registry(&closed_manifest, 1)?;
    assert_eq!(
        registry.complete_records::<FactKeySchema, FactValueSchema>(Vec::new()),
        Err(AuthorityAdmissionError::EmptyFacts)
    );

    let record = FactRecord::<FactKeySchema, FactValueSchema>::new(
        FactKind::Declaration,
        b"symbol".to_vec(),
        b"value".to_vec(),
    );
    let (capability, records) = registry.complete_records(vec![record])?;
    let extraction = Extraction::from_complete_capability(capability, records)?;
    let legacy_epoch = FactEvidence::new(open_authority, &closed_manifest, 1).epoch();
    assert_eq!(extraction.coverage().state(), Coverage::Complete);
    assert!(extraction.records()[0].evidence().is_some_and(|evidence| {
        evidence.authority() == open_authority.digest()
            && evidence.manifest() == closed_manifest.digest()
            && evidence.revision() == 1
            && evidence.epoch() != legacy_epoch
    }));
    Ok(())
}

#[test]
fn raw_complete_labels_cannot_authorize_compile_snapshots() -> Result<(), Box<dyn Error>> {
    let manifest = InputManifest::new(vec![
        Input::new(InputKind::Source, "src/main", b"source")?,
        Input::absent(InputKind::NegativeDependency, "dep/optional")?,
    ])?;
    let forged = forged_complete(&manifest)?;
    let record = FactRecord::<FactKeySchema, FactValueSchema>::new(
        FactKind::Declaration,
        b"symbol".to_vec(),
        b"value".to_vec(),
    );
    assert_eq!(
        FactSnapshot::new(&manifest, 1, forged, vec![record.clone()]),
        Err(FactSnapshotError::UnboundComplete)
    );
    assert_eq!(
        Extraction::bound_records(
            MockAuthority::identity_value(),
            &manifest,
            1,
            forged_complete(&manifest)?,
            vec![record],
        ),
        Err(ExtractionError::UnboundComplete)
    );
    assert_eq!(
        Extraction::bound_at(
            MockAuthority::identity_value(),
            &manifest,
            1,
            forged_complete(&manifest)?,
            vec![FactChange::new(
                "digest-only",
                typed_of::<FactSchema>(b"value")
            )],
        ),
        Err(ExtractionError::UnboundComplete)
    );
    Ok(())
}

#[test]
fn typed_fact_snapshots_produce_scoped_full_and_partial_deltas() -> Result<(), Box<dyn Error>> {
    let manifest = InputManifest::new(vec![Input::new(InputKind::Source, "src/main", b"source")?])?;
    let before_a = FactRecord::<FactKeySchema, FactValueSchema>::new(
        FactKind::Declaration,
        b"a".to_vec(),
        b"one".to_vec(),
    );
    let before_removed = FactRecord::<FactKeySchema, FactValueSchema>::new(
        FactKind::Type,
        b"removed".to_vec(),
        b"old".to_vec(),
    );
    let before =
        complete_test_snapshot(&manifest, 1, vec![before_a.clone(), before_removed.clone()])?;
    let after_a = FactRecord::<FactKeySchema, FactValueSchema>::new(
        FactKind::Declaration,
        b"a".to_vec(),
        b"two".to_vec(),
    );
    let after = complete_test_snapshot(&manifest, 2, vec![after_a.clone()])?;
    let full_delta = before.delta_to(&after)?;
    assert!(
        full_delta
            .changes()
            .iter()
            .any(|change| { matches!(change, FactDeltaChange::Modified { .. }) })
    );
    assert!(full_delta.changes().iter().any(|change| {
        matches!(
            change,
            FactDeltaChange::Removed {
                kind: FactKind::Type,
                ..
            }
        )
    }));
    assert_eq!(full_delta.apply(&before)?, after);
    let stale_base = complete_test_snapshot(
        &manifest,
        1,
        vec![
            FactRecord::<FactKeySchema, FactValueSchema>::new(
                FactKind::Declaration,
                b"a".to_vec(),
                b"stale base with a new value".to_vec(),
            ),
            before_removed.clone(),
        ],
    )?;
    assert_eq!(
        full_delta.apply(&stale_base),
        Err(FactDeltaError::BaseMismatch)
    );

    let partial_after =
        FactSnapshot::new(&manifest, 3, partial_coverage(&manifest), vec![after_a])?;
    let partial_delta = before.delta_to(&partial_after)?;
    assert!(
        !partial_delta
            .changes()
            .iter()
            .any(|change| matches!(change, FactDeltaChange::Removed { .. }))
    );
    let retained = partial_delta.apply(&before)?;
    assert_eq!(retained.records().len(), 2);

    let readded = complete_test_snapshot(
        &manifest,
        4,
        vec![
            FactRecord::<FactKeySchema, FactValueSchema>::new(
                FactKind::Declaration,
                b"a".to_vec(),
                b"two".to_vec(),
            ),
            FactRecord::<FactKeySchema, FactValueSchema>::new(
                FactKind::Type,
                b"removed".to_vec(),
                b"new".to_vec(),
            ),
        ],
    )?;
    let readd_delta = after.delta_to(&readded)?;
    assert!(readd_delta.changes().iter().any(|change| {
        matches!(change, FactDeltaChange::Added(record) if record.key_bytes() == b"removed")
    }));
    assert_eq!(readd_delta.apply(&after)?, readded);

    let other_manifest =
        InputManifest::new(vec![Input::new(InputKind::Source, "src/other", b"other")?])?;
    let other = FactSnapshot::new(
        &other_manifest,
        2,
        partial_coverage(&other_manifest),
        Vec::<FactRecord<FactKeySchema, FactValueSchema>>::new(),
    )?;
    assert_eq!(before.delta_to(&other), Err(FactDeltaError::ScopeMismatch));
    Ok(())
}

#[test]
fn fact_snapshots_share_unchanged_tree_paths() -> Result<(), Box<dyn Error>> {
    let manifest = InputManifest::new(vec![Input::new(InputKind::Source, "src/main", b"source")?])?;
    // Enough rows to exercise the relation kernel's path-copy branches; a
    // tiny relation legitimately fits in one canonical leaf.
    let records = (0..1600)
        .map(|index| {
            FactRecord::<FactKeySchema, FactValueSchema>::new(
                FactKind::Declaration,
                format!("symbol-{index:02}"),
                format!("value-{index:02}"),
            )
        })
        .collect();
    let base = complete_test_snapshot(&manifest, 1, records)?;
    let cloned = base.clone();
    assert!(base.shares_record_storage_with(&cloned));

    let empty = base.delta_to(&base)?.apply(&base)?;
    assert!(base.shares_record_storage_with(&empty));

    let extraction =
        Extraction::from_records(partial_coverage(&manifest), base.records().to_vec())?;
    let extraction_clone = extraction.clone();
    assert!(extraction.shares_record_storage_with(&extraction_clone));

    let mut changed_records = base.records().to_vec();
    let changed_index = changed_records
        .iter()
        .position(|record| record.key_bytes() == b"symbol-117")
        .ok_or("structural sharing fixture key missing")?;
    let changed_key = changed_records[changed_index].key_bytes().to_vec();
    changed_records[changed_index] =
        FactRecord::new(FactKind::Declaration, changed_key, b"replacement".to_vec());
    let target = complete_test_snapshot(&manifest, 2, changed_records)?;
    let applied = base.delta_to(&target)?.apply(&base)?;
    assert_eq!(applied, target);
    assert!(!base.shares_record_storage_with(&applied));
    assert!(base.shared_record_nodes_with(&applied) > 0);
    assert!(base.shared_record_nodes_with(&applied) < base.records().len());
    Ok(())
}

#[test]
fn fact_delta_preserves_evidence_fences_for_unchanged_values() -> Result<(), Box<dyn Error>> {
    let manifest = InputManifest::new(vec![Input::new(InputKind::Source, "src/main", b"source")?])?;
    let before_record = FactRecord::<FactKeySchema, FactValueSchema>::new(
        FactKind::Declaration,
        b"symbol".to_vec(),
        b"same-value".to_vec(),
    );
    let after_record = FactRecord::<FactKeySchema, FactValueSchema>::new(
        FactKind::Declaration,
        b"symbol".to_vec(),
        b"same-value".to_vec(),
    );
    let retained_record = FactRecord::<FactKeySchema, FactValueSchema>::new(
        FactKind::Type,
        b"retained".to_vec(),
        b"unchanged".to_vec(),
    );
    let before = complete_test_snapshot(&manifest, 1, vec![before_record, retained_record])?;
    let retained_record = before
        .records()
        .iter()
        .find(|record| record.key_bytes() == b"retained")
        .cloned()
        .ok_or("retained evidence fixture missing")?;
    let after = complete_test_snapshot(&manifest, 2, vec![after_record, retained_record])?;
    let delta = before.delta_to(&after)?;
    assert!(matches!(
        delta.changes().first(),
        Some(FactDeltaChange::Modified { .. })
    ));
    assert_eq!(delta.apply(&before)?, after);
    Ok(())
}

#[test]
fn fact_delta_advances_revision_without_scanning_or_rebuilding_rows() -> Result<(), Box<dyn Error>>
{
    let manifest = InputManifest::new(vec![Input::new(InputKind::Source, "src/main", b"source")?])?;
    let record = FactRecord::<FactKeySchema, FactValueSchema>::new(
        FactKind::Declaration,
        b"symbol".to_vec(),
        b"value".to_vec(),
    );
    let before = complete_test_snapshot(&manifest, 1, vec![record.clone()])?;
    let retained = before
        .records()
        .first()
        .cloned()
        .ok_or("metadata-only fact fixture missing")?;
    let after = complete_test_snapshot(&manifest, 2, vec![retained])?;
    let delta = before.delta_to(&after)?;
    assert!(delta.changes().is_empty());
    let applied = delta.apply(&before)?;
    assert_eq!(applied, after);
    assert!(before.shares_record_storage_with(&applied));
    Ok(())
}

#[test]
fn authority_change_batch_uses_prepared_relation_and_rejects_partial_omission()
-> Result<(), Box<dyn Error>> {
    let manifest = InputManifest::new(vec![Input::new(InputKind::Source, "src/main", b"source")?])?;
    let before_a = FactRecord::<FactKeySchema, FactValueSchema>::new(
        FactKind::Declaration,
        b"a".to_vec(),
        b"old".to_vec(),
    );
    let before_b = FactRecord::<FactKeySchema, FactValueSchema>::new(
        FactKind::Type,
        b"b".to_vec(),
        b"retained".to_vec(),
    );
    let after_a = FactRecord::<FactKeySchema, FactValueSchema>::new(
        FactKind::Declaration,
        b"a".to_vec(),
        b"new".to_vec(),
    );
    let before = complete_test_snapshot(&manifest, 1, vec![before_a.clone(), before_b.clone()])?;
    let before_b = before
        .records()
        .iter()
        .find(|record| record.key_bytes() == b"b")
        .cloned()
        .ok_or("retained batch fixture missing")?;
    let before_a = before
        .records()
        .iter()
        .find(|record| record.key_bytes() == b"a")
        .cloned()
        .ok_or("changed batch fixture missing")?;
    let after = complete_test_snapshot(&manifest, 2, vec![after_a.clone(), before_b])?;
    let after_a = after
        .records()
        .iter()
        .find(|record| record.key_bytes() == b"a")
        .cloned()
        .ok_or("changed target fixture missing")?;
    let change = FactDeltaChange::Modified {
        before: Box::new(before_a),
        after: Box::new(after_a.clone()),
    };
    let delta = before.delta_from_changes(&after, vec![change.clone()])?;
    assert_eq!(delta.apply(&before)?, after);

    let partial = FactSnapshot::new(&manifest, 3, partial_coverage(&manifest), vec![after_a])?;
    assert_eq!(
        before.delta_from_changes(&partial, vec![change]),
        Err(FactDeltaError::InvalidSnapshot)
    );
    Ok(())
}

#[test]
fn one_record_authority_delta_reuses_100k_fact_state() -> Result<(), Box<dyn Error>> {
    let manifest = InputManifest::new(vec![Input::new(InputKind::Source, "src/main", b"source")?])?;
    let before_records = (0..100_000)
        .map(|index| {
            FactRecord::<FactKeySchema, FactValueSchema>::new(
                FactKind::Declaration,
                format!("symbol-{index:06}"),
                format!("value-{index:06}"),
            )
        })
        .collect::<Vec<_>>();
    let changed_index = 50_000;
    let before = complete_test_snapshot(&manifest, 1, before_records)?;
    let before_changed = before.records()[changed_index].clone();
    let mut after_records = before.records().to_vec();
    let after_changed = FactRecord::<FactKeySchema, FactValueSchema>::new(
        FactKind::Declaration,
        format!("symbol-{changed_index:06}"),
        b"replacement".to_vec(),
    );
    after_records[changed_index] = after_changed.clone();

    let after = complete_test_snapshot(&manifest, 2, after_records)?;
    let after_changed = after
        .records()
        .iter()
        .find(|record| record.key_bytes() == b"symbol-050000")
        .cloned()
        .ok_or("changed target fact missing")?;
    let delta = before.delta_from_changes(
        &after,
        vec![FactDeltaChange::Modified {
            before: Box::new(before_changed),
            after: Box::new(after_changed),
        }],
    )?;
    let prepared = delta.prepare(&before)?;
    let change_count = prepared.delta().changes().len();
    let applied = prepared.commit();
    assert_eq!(applied, after);
    let shared_nodes = before.shared_record_nodes_with(&applied);
    assert!(shared_nodes > 0);
    assert!(shared_nodes < 100_000);
    assert_eq!(
        applied
            .records()
            .iter()
            .find(|record| record.key_bytes() == b"symbol-000000")
            .and_then(FactRecord::evidence)
            .map(|evidence| evidence.revision()),
        Some(1)
    );
    assert_eq!(change_count, 1);
    Ok(())
}

#[test]
fn process_configuration_rejects_ambient_or_unbounded_inputs() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        ProcessEnvironment::new(vec![("A".into(), "1".into()), ("A".into(), "2".into())]),
        Err(ProcessError::DuplicateEnvironment)
    );
    assert_eq!(
        ProcessEnvironment::new(vec![("A\0".into(), "1".into())]),
        Err(ProcessError::InvalidEnvironment)
    );
    assert_eq!(
        ProcessEnvironment::new(vec![("A".into(), "x".repeat(64 * 1024 + 1))]),
        Err(ProcessError::ConfigurationLimit)
    );
    let environment = ProcessEnvironment::new(Vec::new())?;
    let process_limits = limits(1, 1, Duration::from_millis(10), 1)?;
    assert_eq!(
        SupervisedCommand::new(
            PathBuf::from("tool"),
            Vec::new(),
            PathBuf::from("/tmp"),
            environment,
            process_limits,
        ),
        Err(ProcessError::RelativePath)
    );
    assert_eq!(
        SupervisedCommand::new(
            PathBuf::from("/usr/bin/true"),
            vec!["x".repeat(64 * 1024 + 1)],
            PathBuf::from("/tmp"),
            ProcessEnvironment::new(Vec::new())?,
            process_limits,
        ),
        Err(ProcessError::ConfigurationLimit)
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn process_input_and_output_limits_are_independent() -> Result<(), Box<dyn Error>> {
    let input_heavy =
        ProcessLimits::new(1, 1, Duration::from_secs(1), 1)?.with_input_bytes_limit(64)?;
    let process = SupervisedCommand::new(
        PathBuf::from("/usr/bin/true"),
        Vec::new(),
        PathBuf::from("/tmp"),
        ProcessEnvironment::new(Vec::new())?,
        input_heavy,
    )?
    .with_stdin(ProcessStdin::bytes(vec![b'x'; 32]))?;
    assert_eq!(process.run()?.terminal(), ProcessTerminal::Success);

    let output_heavy =
        ProcessLimits::new(64, 64, Duration::from_secs(1), 64)?.with_input_bytes_limit(1)?;
    let process = SupervisedCommand::new(
        PathBuf::from("/bin/sh"),
        vec!["-c".into(), "printf output".into()],
        PathBuf::from("/tmp"),
        ProcessEnvironment::new(Vec::new())?,
        output_heavy,
    )?
    .with_stdin(ProcessStdin::bytes(b"x".to_vec()))?;
    let receipt = process.run()?;
    assert_eq!(receipt.terminal(), ProcessTerminal::Success);
    assert_eq!(receipt.stdout(), b"output");
    Ok(())
}

#[cfg(unix)]
#[test]
fn native_cold_runner_keeps_output_and_coverage_separate() -> Result<(), Box<dyn Error>> {
    let authority = MockAuthority {
        revision: 1,
        unavailable: false,
    };
    let snapshot = authority.discover()?;
    let key = session_key(&authority, &snapshot);
    let command = SupervisedCommand::for_authority(
        PathBuf::from("/bin/sh"),
        vec!["-c".into(), "cat".into()],
        ProcessEnvironment::new(Vec::new())?,
        PathBuf::from("/tmp"),
        ProcessStdin::bytes(b"raw authority bytes"),
        authority.identity().toolchain,
        Some(key),
        ProtocolDescriptor::cold(),
        limits(64, 64, Duration::from_secs(1), 128)?,
    )?;
    let observation = NativeAuthorityRunner::new(command).run_cold()?;
    assert_eq!(observation.stdout(), b"raw authority bytes");
    assert!(observation.stderr().is_empty());
    assert_eq!(observation.exit(), NativeExit::Success(Some(0)));
    assert_eq!(observation.coverage(), Coverage::Unavailable);
    let wrong_manifest =
        InputManifest::new(vec![Input::new(InputKind::Source, "other", b"different")?])?;
    assert_eq!(
        observation
            .clone()
            .with_coverage(forged_complete(&wrong_manifest)?),
        Err(NativeRunnerError::Protocol)
    );
    assert_eq!(
        observation.with_coverage(forged_complete(snapshot.manifest())?),
        Err(NativeRunnerError::Protocol)
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn native_persistent_runner_requires_advertised_protocol_and_key() -> Result<(), Box<dyn Error>> {
    let toolchain = typed_of::<ToolchainSchema>(b"tool");
    let limits = limits(128, 64, Duration::from_secs(1), 192)?;
    let cold = SupervisedCommand::for_authority(
        PathBuf::from("/bin/cat"),
        Vec::new(),
        ProcessEnvironment::new(Vec::new())?,
        PathBuf::from("/tmp"),
        ProcessStdin::null(),
        toolchain,
        None,
        ProtocolDescriptor::cold(),
        limits,
    )?;
    assert!(matches!(
        NativeAuthorityRunner::new(cold).start_persistent(),
        Err(NativeRunnerError::Unsupported)
    ));

    let authority = MockAuthority {
        revision: 1,
        unavailable: false,
    };
    let snapshot = authority.discover()?;
    let command = SupervisedCommand::for_authority(
        PathBuf::from("/bin/cat"),
        Vec::new(),
        ProcessEnvironment::new(Vec::new())?,
        PathBuf::from("/tmp"),
        ProcessStdin::null(),
        authority.identity().toolchain,
        Some(session_key(&authority, &snapshot)),
        ProtocolDescriptor::persistent(),
        limits,
    )?;
    let runner = NativeAuthorityRunner::new(command);
    let session = runner.start_persistent()?;
    assert_eq!(session.state(), SessionState::Ready);
    assert_eq!(session.fallback_to_cold(), Ok(()));

    let mut session = runner.start_persistent()?;
    assert_eq!(
        session.request(snapshot.manifest().digest(), snapshot.sequence()),
        Err(NativeRunnerError::Protocol)
    );
    assert_eq!(session.state(), SessionState::Cold);
    Ok(())
}

#[test]
fn typestate_session_has_one_pending_request_and_exact_response() -> Result<(), Box<dyn Error>> {
    let authority = MockAuthority {
        revision: 7,
        unavailable: false,
    };
    let snapshot = authority.discover()?;
    let key = session_key(&authority, &snapshot).digest();
    let (handshaking, hello) = PersistentSession::<Cold>::new(key).hello();
    let ready = handshaking.ready(hello)?;
    let (pending, _token, request) = ready.request(snapshot.manifest().digest(), 7)?;
    assert_eq!(request.kind(), FrameKind::Request);
    assert_eq!(pending.cancel()?.1.kind(), FrameKind::Cancel);

    let (handshaking, hello) = PersistentSession::<Cold>::new(key).hello();
    let ready = handshaking.ready(hello)?;
    let (pending, token, _) = ready.request(snapshot.manifest().digest(), 7)?;
    let wrong = SessionFrame::response(
        token.sequence().saturating_add(1),
        token.manifest(),
        token.revision(),
    );
    assert!(matches!(
        pending.response(token, wrong),
        Err(ProcessError::Protocol)
    ));
    Ok(())
}

#[test]
fn erased_session_falls_back_after_a_bad_response() -> Result<(), Box<dyn Error>> {
    let authority = MockAuthority {
        revision: 7,
        unavailable: false,
    };
    let snapshot = authority.discover()?;
    let key = session_key(&authority, &snapshot).digest();
    let mut session = ErasedSession::new(key);
    assert_eq!(session.state(), SessionState::Cold);
    let hello = session.hello()?;
    assert_eq!(session.state(), SessionState::Handshaking);
    session.accept_wire(&hello.encode())?;
    let request = session.request(snapshot.manifest().digest(), snapshot.sequence())?;
    assert_eq!(request.kind(), FrameKind::Request);
    let request_sequence = request.sequence().ok_or("request has no sequence")?;
    let bad = SessionFrame::response(
        request_sequence.saturating_add(1),
        snapshot.manifest().digest(),
        snapshot.sequence(),
    );
    assert_eq!(session.accept(bad), Err(ProcessError::Protocol));
    assert_eq!(
        session.accept(SessionFrame::cancel(request_sequence)),
        Err(ProcessError::Protocol)
    );
    assert!(session.requires_cold_fallback());
    session.fallback_to_cold();
    assert_eq!(session.state(), SessionState::Cold);
    assert!(!session.has_pending_request());
    Ok(())
}

#[test]
fn erased_session_rejects_untrusted_wire_identity_before_ready() -> Result<(), Box<dyn Error>> {
    let key = typed_of::<SessionSchema>(b"expected");
    let wrong = SessionFrame::hello(typed_of::<SessionSchema>(b"other"));
    let mut session = ErasedSession::new(key);
    let _ = session.hello()?;
    assert_eq!(
        session.accept_wire(&wrong.encode()),
        Err(ProcessError::Protocol)
    );
    assert_eq!(session.state(), SessionState::Broken);
    Ok(())
}

#[test]
fn frames_round_trip_and_reject_malformed_wire_data() -> Result<(), Box<dyn Error>> {
    let manifest = InputManifest::new(vec![Input::new(InputKind::Source, "main", b"x")?])?;
    let session = typed_of::<SessionSchema>(b"session");
    let frames = [
        SessionFrame::hello(session),
        SessionFrame::request(2, manifest.digest(), 9),
        SessionFrame::cancel(2),
        SessionFrame::reset(),
        SessionFrame::response(2, manifest.digest(), 9),
    ];
    for frame in frames {
        let decoded = match frame.kind() {
            FrameKind::Hello => SessionFrame::decode_for(&frame.encode(), Some(session), None)?,
            FrameKind::Request | FrameKind::Response => {
                SessionFrame::decode_for(&frame.encode(), None, Some(manifest.digest()))?
            }
            FrameKind::Cancel | FrameKind::Reset => {
                SessionFrame::decode(&frame.encode())?.admit(None, None)?
            }
        };
        assert_eq!(decoded, frame);
    }

    let hello_bytes = SessionFrame::hello(session).encode();
    assert_eq!(
        SessionFrame::decode(&hello_bytes)?.admit(None, None),
        Err(FrameError::InvalidIdentity)
    );
    let wrong_session = typed_of::<SessionSchema>(b"other-session");
    assert_eq!(
        SessionFrame::decode_for(&hello_bytes, Some(wrong_session), None),
        Err(FrameError::InvalidIdentity)
    );

    let mut truncated = SessionFrame::reset().encode();
    truncated.pop();
    assert_eq!(SessionFrame::decode(&truncated), Err(FrameError::Truncated));
    let mut wrong_magic = SessionFrame::reset().encode();
    wrong_magic[0] = b'X';
    assert_eq!(
        SessionFrame::decode(&wrong_magic),
        Err(FrameError::InvalidMagic)
    );
    let mut wrong_version = SessionFrame::reset().encode();
    wrong_version[4] = 2;
    assert_eq!(
        SessionFrame::decode(&wrong_version),
        Err(FrameError::UnsupportedVersion(2))
    );
    let mut trailing = SessionFrame::reset().encode();
    trailing.push(0);
    assert_eq!(
        SessionFrame::decode(&trailing),
        Err(FrameError::InvalidLength {
            expected: 6,
            actual: 7,
        })
    );
    let mut unknown = SessionFrame::reset().encode();
    unknown[5] = 255;
    assert_eq!(
        SessionFrame::decode(&unknown),
        Err(FrameError::UnknownKind(255))
    );
    Ok(())
}

#[test]
fn pool_leases_are_bounded_and_return_scratch_on_drop() -> Result<(), Box<dyn Error>> {
    let pool = BufferPool::new(1, 8)?;
    let mut lease = pool.acquire(4)?;
    lease.extend_from_slice(b"abcd")?;
    assert_eq!(lease.as_slice(), b"abcd");
    assert_eq!(pool.stats().live, 1);
    assert!(matches!(pool.acquire(1), Err(PoolError::Exhausted)));
    assert_eq!(
        lease.extend_from_slice(b"efghij"),
        Err(PoolError::BufferLimit)
    );
    drop(lease);
    assert_eq!(
        pool.stats(),
        PoolStats {
            live: 0,
            available: 1
        }
    );
    let reused = pool.acquire(2)?;
    assert!(reused.is_empty());
    Ok(())
}

#[cfg(unix)]
fn command(
    program: &str,
    args: &[&str],
    process_limits: ProcessLimits,
) -> Result<SupervisedCommand, ProcessError> {
    SupervisedCommand::new(
        PathBuf::from(program),
        args.iter().map(|arg| (*arg).to_owned()).collect(),
        PathBuf::from("/tmp"),
        ProcessEnvironment::new(Vec::new())?,
        process_limits,
    )
}

#[cfg(unix)]
#[test]
fn supervisor_returns_a_reaped_bounded_success_receipt() -> Result<(), Box<dyn Error>> {
    let process_limits = limits(32, 32, Duration::from_secs(1), 64)?;
    let process = command("/bin/sh", &["-c", "printf hello"], process_limits)?;
    let running = ProcessSupervisor::new(process).start()?;
    let receipt = running.finish()?;
    assert_eq!(receipt.terminal(), ProcessTerminal::Success);
    assert_eq!(receipt.status(), Some(0));
    assert_eq!(receipt.stdout(), b"hello");
    assert!(receipt.stderr().is_empty());
    assert!(receipt.reaped());
    Ok(())
}

#[cfg(unix)]
#[test]
fn supervisor_kills_on_output_limit_and_deadline() -> Result<(), Box<dyn Error>> {
    let output_limits = limits(3, 3, Duration::from_secs(1), 6)?;
    let output_process = command("/bin/sh", &["-c", "printf 123456"], output_limits)?;
    assert_eq!(output_process.run(), Err(ProcessError::OutputLimit));

    let deadline_limits = limits(32, 32, Duration::from_millis(20), 64)?;
    let sleep_process = command("/bin/sleep", &["1"], deadline_limits)?;
    assert_eq!(sleep_process.run(), Err(ProcessError::Deadline));
    Ok(())
}

#[cfg(unix)]
#[test]
fn supervisor_kills_when_the_caller_cancels() -> Result<(), Box<dyn Error>> {
    let process_limits = limits(32, 32, Duration::from_secs(2), 64)?;
    let process = command("/bin/sleep", &["1"], process_limits)?;
    let (cancellation, handle) = Cancellation::new();
    let join = thread::spawn(move || process.run_with_cancellation(&cancellation));
    thread::sleep(Duration::from_millis(20));
    handle.cancel();
    let result = join.join().map_err(|_| "supervisor thread panicked")?;
    assert_eq!(result, Err(ProcessError::Cancelled));
    Ok(())
}

#[cfg(unix)]
#[test]
fn supervisor_cancels_the_entire_process_group() -> Result<(), Box<dyn Error>> {
    let pid_path = std::env::temp_dir().join(format!(
        "backend-compile-grandchild-{}.pid",
        std::process::id()
    ));
    let _ = fs::remove_file(&pid_path);
    let pid_tmp_path = pid_path.with_extension("pid.tmp");
    let _ = fs::remove_file(&pid_tmp_path);
    let script = format!(
        "sleep 30 & child=$!; printf '%s' \"$child\" > {}; mv {} {}; wait",
        pid_tmp_path.display(),
        pid_tmp_path.display(),
        pid_path.display(),
    );
    let process_limits = limits(64, 64, Duration::from_secs(2), 128)?;
    let process = command("/bin/sh", &["-c", &script], process_limits)?;
    let (cancellation, handle) = Cancellation::new();
    let join = thread::spawn(move || process.run_with_cancellation(&cancellation));
    // The shell creates/truncates the file before `printf` writes the PID;
    // existence alone therefore races with the reader. Wait for the complete
    // parseable value, which is the actual child-start handshake.
    let child_pid = (0..100)
        .find_map(|_| {
            let result = fs::read_to_string(&pid_path)
                .ok()
                .and_then(|pid| pid.trim().parse::<u32>().ok());
            if result.is_none() {
                thread::sleep(Duration::from_millis(2));
            }
            result
        })
        .ok_or("grandchild PID was not published")?;
    handle.cancel();
    let result = join.join().map_err(|_| "supervisor thread panicked")?;
    assert_eq!(result, Err(ProcessError::Cancelled));
    for _ in 0..100 {
        let alive = Command::new("/bin/kill")
            .arg("-0")
            .arg(child_pid.to_string())
            .status()
            .is_ok_and(|status| status.success());
        if !alive {
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    let alive = Command::new("/bin/kill")
        .arg("-0")
        .arg(child_pid.to_string())
        .status()
        .is_ok_and(|status| status.success());
    let _ = fs::remove_file(pid_path);
    let _ = fs::remove_file(pid_tmp_path);
    assert!(!alive);
    Ok(())
}

#[cfg(unix)]
#[test]
fn supervisor_enforces_workspace_growth() -> Result<(), Box<dyn Error>> {
    let workspace =
        std::env::temp_dir().join(format!("backend-compile-workspace-{}", std::process::id()));
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir(&workspace)?;
    let process_limits = limits(64, 64, Duration::from_secs(1), 128)?.with_workspace_limit(4)?;
    let process = SupervisedCommand::new(
        PathBuf::from("/bin/sh"),
        vec!["-c".into(), "printf 12345 > created".into()],
        workspace.clone(),
        ProcessEnvironment::new(Vec::new())?,
        process_limits,
    )?;
    assert_eq!(process.run(), Err(ProcessError::WorkspaceLimit));
    let _ = fs::remove_dir_all(workspace);
    Ok(())
}

#[cfg(unix)]
#[test]
fn supervisor_enforces_unix_process_count_limit() -> Result<(), Box<dyn Error>> {
    let process_limits =
        limits(64, 64, Duration::from_secs(2), 128)?.with_process_count_limit(1)?;
    let process = command("/bin/sh", &["-c", "sleep 1 & wait"], process_limits)?;
    let receipt = process.run()?;
    assert_eq!(receipt.terminal(), ProcessTerminal::Exit);
    assert_ne!(receipt.status(), Some(0));
    assert!(receipt.reaped());
    Ok(())
}

#[cfg(unix)]
#[test]
fn supervisor_enforces_unix_cpu_time_limit() -> Result<(), Box<dyn Error>> {
    let process_limits =
        limits(64, 64, Duration::from_secs(3), 128)?.with_cpu_time_limit(Duration::from_secs(1))?;
    let process = command("/bin/sh", &["-c", "while :; do :; done"], process_limits)?;
    let receipt = process.run()?;
    assert_eq!(receipt.terminal(), ProcessTerminal::Exit);
    assert_ne!(receipt.status(), Some(0));
    assert!(receipt.reaped());
    Ok(())
}

#[cfg(unix)]
#[test]
fn unsupported_resource_bounds_are_reported_before_spawn() -> Result<(), Box<dyn Error>> {
    let process_limits = limits(32, 32, Duration::from_secs(1), 64)?.with_memory_bytes_limit(1)?;
    let process = command("/bin/true", &[], process_limits)?;
    assert_eq!(
        process.run(),
        Err(ProcessError::UnsupportedLimit(
            UnsupportedLimit::MemoryBytes
        ))
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn executable_identity_rejects_same_path_replacement() -> Result<(), Box<dyn Error>> {
    use std::os::unix::fs::PermissionsExt;

    let workspace =
        std::env::temp_dir().join(format!("backend-compile-executable-{}", std::process::id()));
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir(&workspace)?;
    let executable = workspace.join("authority.sh");
    fs::write(&executable, b"#!/bin/sh\nprintf old\n")?;
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))?;
    let artifact = ToolchainArtifact::from_path(&executable, vec![typed_of(b"sdk")])?;
    let process_limits = limits(32, 32, Duration::from_secs(1), 64)?;
    let process = SupervisedCommand::for_authority_with_artifact(
        executable.clone(),
        Vec::new(),
        ProcessEnvironment::new(Vec::new())?,
        workspace.clone(),
        ProcessStdin::null(),
        artifact,
        None,
        ProtocolDescriptor::cold(),
        process_limits,
    )?;
    fs::write(&executable, b"#!/bin/sh\nprintf new\n")?;
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))?;
    assert_eq!(process.run(), Err(ProcessError::ExecutableDrift));
    let _ = fs::remove_dir_all(workspace);
    Ok(())
}

#[test]
fn executable_identity_rejects_sparse_oversize_before_allocation() -> Result<(), Box<dyn Error>> {
    let executable = std::env::temp_dir().join(format!(
        "backend-compile-oversized-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    let file = fs::File::create(&executable)?;
    file.set_len(MAX_EXECUTABLE_BYTES + 1)?;
    assert_eq!(
        ExecutableIdentity::from_path(&executable),
        Err(ProcessError::ExecutableLimit)
    );
    fs::remove_file(executable)?;
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn executable_lease_isolated_from_in_place_mutation() -> Result<(), Box<dyn Error>> {
    use std::os::unix::fs::PermissionsExt;

    let workspace = std::env::temp_dir().join(format!(
        "backend-compile-executable-lease-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir(&workspace)?;
    let executable = workspace.join("authority.sh");
    fs::write(&executable, b"#!/bin/sh\nprintf old\n")?;
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))?;
    let artifact = ToolchainArtifact::from_path(&executable, Vec::new())?;
    let command = SupervisedCommand::for_authority_with_artifact(
        executable.clone(),
        Vec::new(),
        ProcessEnvironment::new(Vec::new())?,
        workspace.clone(),
        ProcessStdin::null(),
        artifact,
        None,
        ProtocolDescriptor::cold(),
        limits(32, 32, Duration::from_secs(1), 64)?,
    )?;
    let lease = process::ExecutableLease::prepare(&command)?;
    assert!(
        fs::OpenOptions::new()
            .write(true)
            .open(lease.path())
            .is_err()
    );
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o777))?;
    fs::write(&executable, b"#!/bin/sh\nprintf changed\n")?;
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))?;
    let output = Command::new(lease.path()).output()?;
    assert_eq!(output.stdout, b"old");
    drop(lease);
    let _ = fs::remove_dir_all(workspace);
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn executable_lease_copies_macho_before_original_mutation() -> Result<(), Box<dyn Error>> {
    use std::os::unix::fs::PermissionsExt;

    let source = PathBuf::from("/usr/bin/true");
    if !source.is_file() {
        return Ok(());
    }
    let workspace = std::env::temp_dir().join(format!(
        "backend-compile-macho-lease-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir(&workspace)?;
    let executable = workspace.join("authority");
    fs::copy(&source, &executable)?;
    // Keep the copied image owner-writable. The lease must still isolate the
    // already verified bytes from an in-place mutation of this inode.
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))?;
    let artifact = ToolchainArtifact::from_path(&executable, Vec::new())?;
    let command = SupervisedCommand::for_authority_with_artifact(
        executable.clone(),
        Vec::new(),
        ProcessEnvironment::new(Vec::new())?,
        workspace.clone(),
        ProcessStdin::null(),
        artifact,
        None,
        ProtocolDescriptor::cold(),
        limits(32, 32, Duration::from_secs(1), 64)?,
    )?;
    let lease = process::ExecutableLease::prepare(&command)?;
    assert_ne!(lease.path(), executable.as_path());
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o777))?;
    let mut original = fs::OpenOptions::new().write(true).open(&executable)?;
    original.write_all(b"mutated")?;
    original.flush()?;
    let output = Command::new(lease.path()).output()?;
    assert!(output.status.success());
    drop(lease);
    let _ = fs::remove_dir_all(workspace);
    Ok(())
}

#[cfg(unix)]
#[test]
fn persistent_observation_separates_frame_and_validated_payload() -> Result<(), Box<dyn Error>> {
    let python = [
        "/usr/bin/python3",
        "/usr/local/bin/python3",
        "/opt/homebrew/bin/python3",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|path| path.is_file());
    let Some(python) = python else {
        return Ok(());
    };
    let authority = MockAuthority {
        revision: 1,
        unavailable: false,
    };
    let snapshot = authority.discover()?;
    let key = session_key(&authority, &snapshot);
    let payload = NativeEnvelope::unbound(
        "rust",
        [1; 32],
        [2; 32],
        [3; 32],
        1,
        NativeCoverage::Partial,
        vec![NativeRecord::new(
            NativeRecordKind::Declaration,
            "symbol",
            b"value",
        )?],
    )?
    .encode()?;
    let mut payload_hex = String::with_capacity(payload.len().saturating_mul(2));
    for byte in &payload {
        let _ = write!(&mut payload_hex, "{byte:02x}");
    }
    let workspace =
        std::env::temp_dir().join(format!("backend-compile-persistent-{}", std::process::id()));
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir(&workspace)?;
    let script_path = workspace.join("authority.py");
    fs::write(
        &script_path,
        br#"import os, sys
def read_exact(size):
    data = b""
    while len(data) < size:
        part = sys.stdin.buffer.read(size - len(data))
        if not part:
            return data
        data += part
    return data
hello = read_exact(38)
if len(hello) != 38:
    raise SystemExit(1)
sys.stdout.buffer.write(hello)
sys.stdout.buffer.flush()
payload = bytes.fromhex(os.environ["BACKEND_PAYLOAD"])
while True:
    request = read_exact(54)
    if not request:
        break
    if len(request) != 54:
        raise SystemExit(1)
    response = bytearray(request)
    response[5] = 5
    sys.stdout.buffer.write(response)
    sys.stdout.buffer.flush()
    sys.stderr.buffer.write(payload)
    sys.stderr.buffer.flush()
"#,
    )?;
    let environment = ProcessEnvironment::new(vec![("BACKEND_PAYLOAD".into(), payload_hex)])?;
    let process_limits = limits(128, 4 * 1024, Duration::from_secs(2), 8 * 1024)?;
    let command = SupervisedCommand::for_authority(
        python,
        vec![script_path.to_string_lossy().into_owned()],
        environment,
        workspace.clone(),
        ProcessStdin::null(),
        authority.identity().toolchain,
        Some(key),
        ProtocolDescriptor::persistent(),
        process_limits,
    )?;
    let runner = NativeAuthorityRunner::new(command);
    let mut session = runner.start_persistent()?;
    let observation = session.request(snapshot.manifest().digest(), snapshot.sequence())?;
    assert_eq!(observation.frame().len(), 54);
    assert_eq!(observation.frame()[5], 5);
    assert!(observation.stdout().starts_with(b"BCF"));
    let validated = observation.payload().ok_or("missing payload")?;
    assert_eq!(validated.records()[0].key(), "symbol");
    assert_eq!(observation.decode_payload()?, validated.clone());
    session.fallback_to_cold()?;
    let _ = fs::remove_dir_all(workspace);
    Ok(())
}

#[cfg(unix)]
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "The persistent fixture journey intentionally keeps setup, serial, concurrent, and retirement assertions together."
)]
fn persistent_session_cache_native_fixture_reuses_one_pid_across_serial_and_concurrent_requests()
-> Result<(), Box<dyn Error>> {
    let python = [
        "/usr/bin/python3",
        "/usr/local/bin/python3",
        "/opt/homebrew/bin/python3",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|path| path.is_file());
    let Some(python) = python else {
        return Ok(());
    };
    let authority = MockAuthority {
        revision: 1,
        unavailable: false,
    };
    let snapshot = authority.discover()?;
    let manifest_id = snapshot.manifest().digest();
    let revision = snapshot.sequence();
    let key = session_key(&authority, &snapshot);
    let payload = NativeEnvelope::unbound(
        "rust",
        [11; 32],
        [12; 32],
        [13; 32],
        1,
        NativeCoverage::Partial,
        vec![NativeRecord::new(
            NativeRecordKind::Declaration,
            "persistent-symbol",
            b"same-semantic-result",
        )?],
    )?
    .encode()?;
    let expected = NativeEnvelope::decode(&payload)?;
    let mut payload_hex = String::with_capacity(payload.len().saturating_mul(2));
    for byte in &payload {
        let _ = write!(&mut payload_hex, "{byte:02x}");
    }
    let workspace = std::env::temp_dir().join(format!(
        "backend-compile-persistent-cache-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir(&workspace)?;
    let script_path = workspace.join("authority.py");
    let pid_path = workspace.join("pid");
    let pid_tmp_path = workspace.join("pid.tmp");
    fs::write(
        &script_path,
        br#"import os, sys
import resource
def read_exact(size):
    data = b""
    while len(data) < size:
        part = sys.stdin.buffer.read(size - len(data))
        if not part:
            return data
        data += part
    return data
pid_tmp = os.environ["BACKEND_PID_TMP"]
pid_path = os.environ["BACKEND_PID"]
rss_path = os.environ["BACKEND_RSS"]
with open(pid_tmp, "w") as marker:
    marker.write(str(os.getpid()))
    marker.flush()
    os.fsync(marker.fileno())
os.replace(pid_tmp, pid_path)
hello = read_exact(38)
if len(hello) != 38:
    raise SystemExit(1)
sys.stdout.buffer.write(hello)
sys.stdout.buffer.flush()
payload = bytes.fromhex(os.environ["BACKEND_PAYLOAD"])
while True:
    request = read_exact(54)
    if not request:
        break
    if len(request) != 54:
        raise SystemExit(1)
    response = bytearray(request)
    response[5] = 5
    sys.stdout.buffer.write(response)
    sys.stdout.buffer.flush()
    sys.stderr.buffer.write(payload)
    sys.stderr.buffer.flush()
    rss = 0
    try:
        with open("/proc/self/status") as status:
            for line in status:
                if line.startswith("VmHWM:"):
                    rss = int(line.split()[1]) * 1024
                    break
    except OSError:
        rss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
        if sys.platform != "darwin":
            rss *= 1024
    with open(rss_path, "a") as measurement:
        measurement.write(str(rss) + "\n")
"#,
    )?;
    let environment = ProcessEnvironment::new(vec![
        ("BACKEND_PAYLOAD".into(), payload_hex),
        (
            "BACKEND_PID".into(),
            pid_path.to_string_lossy().into_owned(),
        ),
        (
            "BACKEND_PID_TMP".into(),
            pid_tmp_path.to_string_lossy().into_owned(),
        ),
        (
            "BACKEND_RSS".into(),
            workspace.join("rss").to_string_lossy().into_owned(),
        ),
    ])?;
    // Persistent stdout/stderr accounting is cumulative for the lifetime of
    // the process, so the fixture bound covers all forty audited responses
    // while remaining bounded well below the protocol maximum.
    let process_limits = limits(4 * 1024, 12 * 1024, Duration::from_secs(2), 16 * 1024)?;
    let command = SupervisedCommand::for_authority(
        python,
        vec![script_path.to_string_lossy().into_owned()],
        environment,
        workspace.clone(),
        ProcessStdin::null(),
        authority.identity().toolchain,
        Some(key),
        ProtocolDescriptor::persistent(),
        process_limits,
    )?;
    let runner = Arc::new(NativeAuthorityRunner::new(command));
    let config = SessionCacheConfig::new(1)?;
    let cache = PersistentSessionCache::new(config);
    let preparation_key = PreparationKey::new(
        authority.identity(),
        manifest_id,
        key,
        revision,
        "rust",
        &[],
    )?;
    let request = |cache: &PersistentSessionCache,
                   runner: &NativeAuthorityRunner|
     -> Result<NativeObservation, SessionCacheError> {
        let (cancellation, _handle) = Cancellation::new();
        cache.request(PersistentRequest::new(
            preparation_key,
            runner,
            manifest_id,
            revision,
            &[],
            &cancellation,
        ))
    };
    // Keep one process alive through both request phases. Every result is
    // decoded and compared to the same semantic fixture payload.
    for _ in 0..20 {
        let observation = request(&cache, &runner)?;
        assert_eq!(observation.payload(), Some(&expected));
        assert!(observation.stdout().len() <= process_limits.stdout());
        assert!(observation.stderr().len() <= process_limits.stderr());
        assert!(
            observation.stdout().len() + observation.stderr().len()
                <= process_limits.output_bytes()
        );
    }
    let runner_threads = Arc::clone(&runner);
    let cache_threads = cache.clone();
    let mut workers = Vec::with_capacity(20);
    for _ in 0..20 {
        let runner = Arc::clone(&runner_threads);
        let cache = cache_threads.clone();
        workers.push(thread::spawn(move || {
            let (cancellation, _handle) = Cancellation::new();
            cache.request(PersistentRequest::new(
                preparation_key,
                &runner,
                manifest_id,
                revision,
                &[],
                &cancellation,
            ))
        }));
    }
    for worker in workers {
        let observation = worker.join().map_err(|_| "persistent worker panicked")??;
        assert_eq!(observation.payload(), Some(&expected));
        assert!(observation.stdout().len() <= process_limits.stdout());
        assert!(observation.stderr().len() <= process_limits.stderr());
        assert!(
            observation.stdout().len() + observation.stderr().len()
                <= process_limits.output_bytes()
        );
    }
    let stats = cache.stats();
    assert_eq!(stats.starts, 1, "one process start per SessionKey");
    assert_eq!(stats.hits, 39, "all remaining requests reuse the process");
    assert_eq!(stats.resident, 1);
    let mut rss_values = Vec::new();
    for _ in 0..100 {
        rss_values = fs::read_to_string(workspace.join("rss"))?
            .lines()
            .map(str::parse::<u64>)
            .collect::<Result<Vec<_>, _>>()?;
        if rss_values.len() >= 40 {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(rss_values.len(), 40);
    let peak_rss = rss_values.iter().copied().max().unwrap_or(0);
    assert!(peak_rss > 0);
    assert!(
        peak_rss <= 64 * 1024 * 1024,
        "fixture RSS exceeded bound: {peak_rss}"
    );
    eprintln!(
        "persistent fixture starts={} hits={} resident={} peak_rss_bytes={} stdout_bound={} stderr_bound={} aggregate_bound={}",
        stats.starts,
        stats.hits,
        stats.resident,
        peak_rss,
        process_limits.stdout(),
        process_limits.stderr(),
        process_limits.output_bytes(),
    );
    let pid = (0..100)
        .find_map(|_| {
            fs::read_to_string(&pid_path)
                .ok()
                .and_then(|value| value.trim().parse::<u32>().ok())
        })
        .ok_or("persistent fixture PID was not published")?;
    assert!(cache.invalidate(preparation_key));
    for _ in 0..100 {
        let alive = Command::new("/bin/kill")
            .arg("-0")
            .arg(pid.to_string())
            .status()
            .is_ok_and(|status| status.success());
        if !alive {
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    let alive = Command::new("/bin/kill")
        .arg("-0")
        .arg(pid.to_string())
        .status()
        .is_ok_and(|status| status.success());
    assert!(!alive, "retired persistent authority remained alive");
    assert_eq!(cache.stats().resident, 0);
    let _ = fs::remove_dir_all(workspace);
    Ok(())
}
