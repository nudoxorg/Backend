//! Configuration belongs to one live host and one registry composition.

#![allow(clippy::expect_used)]

use super::*;
use crate::core::VersionedRoot;
use crate::host::lease::OwnerConfiguration;
use crate::model::ServiceMode;
use crate::runtime::owner::{OwnerFault, OwnerGate, OwnerState, PublicationAdmission};
use backend_library::{
    AuthorityScopeClaim, Basis, CoverageCapability, Cursor, Frontier, ProducerObservationClaims,
    ProducerObservationVerifier, ScopeRoot, UntrustedProducerObservation, ViewRoot,
    admit_complete_scope, admit_producer_observation, object_version, view_key, view_state_root,
};
use backend_local_service::{ClosedLocalHostEnvironmentSnapshot, LocalHostVariable, LocaldRuntimePolicy};

fn configuration(rustc: &str) -> OwnerConfiguration {
    let path = PathBuf::from(rustc);
    let path = if path.is_absolute() { path } else {
        std::env::temp_dir().join(rustc.trim_start_matches('/'))
    };
    OwnerConfiguration {
        compiler_environment: ClosedLocalHostEnvironmentSnapshot::from_paths([
            (LocalHostVariable::NudoxRustc, path),
        ]).expect("closed owner fixture"),
        runtime_policy: LocaldRuntimePolicy::parse(
            r#"{"version":1,"registry_network_allowed":false,"discovery_network_allowed":false,"advisory_network_allowed":false,"advisory_refresh_enabled":false,"registry_cache_max_age_millis":0}"#,
        ).expect("canonical restrictive owner policy"),
    }
}

fn composition(context: Option<OwnerContext>) -> ServingComposition {
    let registry = Composition {
        endpoint: PathBuf::from("/same/owner.sock"),
        source: Arc::new(CargoCache::at(
            PathBuf::from("/same/cargo"),
            PathBuf::from("/same/data"),
        )),
        authority: Arc::from("same-cargo-authority"),
        generation: CompositionGeneration::default(),
        refusals: None,
    };
    ServingComposition {
        endpoint: registry.endpoint.clone(),
        data: Some(PathBuf::from("/same/data")),
        generation: CompositionGeneration::default(),
        registry: Some(registry),
        owner_context: context,
    }
}

fn key() -> VersionedRoot {
    let root = root();
    VersionedRoot::from_revision(1, Cursor::for_view_root_at(&root, 0), 0)
}

fn current(slot: &RwLock<Option<ServingComposition>>) -> ServingComposition {
    slot.read()
        .expect("composition read")
        .as_ref()
        .expect("installed composition")
        .clone()
}

fn context(lifetime: &Arc<()>, gate: &OwnerGate, rustc: &str) -> OwnerContext {
    OwnerContext::new(configuration(rustc), Arc::downgrade(lifetime), gate.clone())
}

#[test]
fn owner_configuration_and_paths_survive_without_a_cargo_source() {
    let slot = RwLock::new(None);
    let lifetime = Arc::new(());
    let gate = OwnerGate::ready(key(), ServiceMode::Embedded);
    let mut configuration = configuration("/unused/rustc");
    configuration.compiler_environment = ClosedLocalHostEnvironmentSnapshot::from_paths([
        (LocalHostVariable::NudoxDotnet, std::env::temp_dir().join("fixture-dotnet")),
    ]).expect("C# owner has no Cargo or home selection");
    let data = Path::new("/actual/workspace");
    assert!(CargoCache::from_snapshot(&configuration.compiler_environment, data.join("registry-sources")).is_none());
    let _lease = lease_into(&slot, compose(
        Path::new("/actual/owner.sock"), data, None,
        Some(OwnerContext::new(configuration.clone(), Arc::downgrade(&lifetime), gate)),
    ));
    let serving = current(&slot);
    assert!(serving.registry.is_none(), "no fake source capability");
    assert_eq!(serving.endpoint, Path::new("/actual/owner.sock"));
    assert_eq!(serving.data.as_deref(), Some(data), "workspace is direct, independent of refusal paths");
    assert_eq!(serving.configuration_in(&slot).expect("live non-Cargo owner").compiler_environment,
        configuration.compiler_environment);
}

#[test]
fn replacement_without_cargo_withdraws_the_old_source_but_keeps_current_owner_context() {
    let slot = RwLock::new(None);
    let old_lifetime = Arc::new(());
    let new_lifetime = Arc::new(());
    let gate = OwnerGate::ready(key(), ServiceMode::Embedded);
    let old_lease = lease_into(&slot, composition(Some(context(&old_lifetime, &gate, "/old/rustc"))));
    let old = current(&slot);
    let mut replacement = composition(Some(context(&new_lifetime, &gate, "/new/rustc")));
    replacement.registry = None;
    let _new_lease = lease_into(&slot, replacement);
    drop(old_lease);
    let new = current(&slot);
    assert!(new.registry.is_none(), "old Cargo capability cannot leak through replacement");
    assert!(old.configuration_in(&slot).is_none());
    assert_eq!(new.configuration_in(&slot).expect("held replacement").compiler_environment,
        configuration("/new/rustc").compiler_environment);
}

#[test]
fn cargo_authority_uses_only_the_closed_selection_and_existing_default_cache() {
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .expect("clock").as_nanos();
    let root = std::env::temp_dir().join(format!("nudox-closed-cargo-{}-{nonce}", std::process::id()));
    let home = root.join("home");
    let selected = root.join("selected-cargo");
    let explicit_missing = root.join("configured-missing");
    std::fs::create_dir_all(&home).expect("owned home fixture");
    std::fs::create_dir_all(&selected).expect("selected Cargo fixture");
    let snapshot = |paths: Vec<_>| ClosedLocalHostEnvironmentSnapshot::from_paths(paths).expect("closed cache fixture");
    let absent = snapshot(vec![]);
    assert!(CargoCache::from_snapshot(&absent, root.join("unpacked")).is_none(), "ambient HOME cannot fill closed absence");
    let by_home = snapshot(vec![(LocalHostVariable::Home, home.clone())]);
    assert!(CargoCache::from_snapshot(&by_home, root.join("unpacked")).is_none(), "an inferred missing directory is not a source capability");
    assert!(!home.join(".cargo").exists(), "registry composition does not realize a default");
    std::fs::create_dir(home.join(".cargo")).expect("existing default cache");
    let existing = CargoCache::from_snapshot(&by_home, root.join("unpacked")).expect("existing default cache");
    assert_eq!(existing.home, stable_path(home.join(".cargo")));
    let configured = snapshot(vec![(LocalHostVariable::Home, home.clone()),
        (LocalHostVariable::NudoxCargoHome, selected.clone())]);
    let exact = CargoCache::from_snapshot(&configured, root.join("unpacked")).expect("selected authority");
    assert_eq!(exact.home, stable_path(selected));
    assert_ne!(exact.authority_key(), existing.authority_key(), "selected authority cannot fall through to another HOME cache");
    let missing = snapshot(vec![(LocalHostVariable::Home, home.clone()),
        (LocalHostVariable::NudoxCargoHome, explicit_missing.clone())]);
    assert!(CargoCache::from_snapshot(&missing, root.join("unpacked")).is_none(), "explicit missing home must not use the existing default");
    assert!(!explicit_missing.exists(), "configured paths are not realized");
    let invalid_root = snapshot(vec![(LocalHostVariable::Home, home),
        (LocalHostVariable::NudoxCargoRoot, explicit_missing)]);
    assert!(CargoCache::from_snapshot(&invalid_root, root.join("unpacked")).is_none(), "explicit missing source authority cannot be ignored");
    std::fs::remove_dir_all(root).expect("owned fixture cleanup");
}

#[test]
fn same_endpoint_replacement_revokes_old_snapshot_and_old_drop_keeps_new_host() {
    let slot = RwLock::new(None);
    let first_lifetime = Arc::new(());
    let second_lifetime = Arc::new(());
    let gate = OwnerGate::ready(key(), ServiceMode::Embedded);
    let first_lease = lease_into(
        &slot,
        composition(Some(context(&first_lifetime, &gate, "/first/rustc"))),
    );
    let first = current(&slot);
    assert_eq!(
        first
            .configuration_in(&slot)
            .expect("first owner")
            .compiler_environment,
        configuration("/first/rustc").compiler_environment
    );

    let second_lease = lease_into(
        &slot,
        composition(Some(context(&second_lifetime, &gate, "/second/rustc"))),
    );
    let second = current(&slot);
    assert_eq!(first.endpoint, second.endpoint);
    assert_eq!(first.registry.as_ref().expect("first actual source").authority,
        second.registry.as_ref().expect("second actual source").authority);
    assert_eq!(first.registry.as_ref().expect("first actual source").generation, first.generation);
    assert_eq!(second.registry.as_ref().expect("second actual source").generation, second.generation);
    assert_ne!(first.generation, second.generation);
    assert!(
        first.configuration_in(&slot).is_none(),
        "same paths cannot revive an old owner's compiler selection"
    );
    drop(first_lease);
    assert_eq!(
        second
            .configuration_in(&slot)
            .expect("new owner survives old drop")
            .compiler_environment,
        configuration("/second/rustc").compiler_environment
    );
    drop(second_lease);
    assert!(slot.read().expect("withdrawn composition").is_none());
    assert!(
        second.configuration_in(&slot).is_none(),
        "a retained settings snapshot does not retain the owner lease"
    );
}

#[test]
fn a_dead_host_cannot_export_from_a_retained_composition() {
    let slot = RwLock::new(None);
    let lifetime = Arc::new(());
    let gate = OwnerGate::ready(key(), ServiceMode::Embedded);
    let _lease = lease_into(
        &slot,
        composition(Some(context(&lifetime, &gate, "/held/rustc"))),
    );
    let snapshot = current(&slot);
    assert!(snapshot.configuration_in(&slot).is_some());
    drop(lifetime);
    assert!(
        snapshot.configuration_in(&slot).is_none(),
        "the composition and settings snapshot hold only a weak host lifetime"
    );
}

#[test]
fn starting_failed_panicked_lost_and_closed_owners_do_not_export() {
    let slot = RwLock::new(None);
    let lifetime = Arc::new(());
    let gate = OwnerGate::starting();
    let _lease = lease_into(
        &slot,
        composition(Some(context(&lifetime, &gate, "/held/rustc"))),
    );
    let snapshot = current(&slot);
    assert!(
        snapshot.configuration_in(&slot).is_none(),
        "revision proof must precede Ready"
    );
    for fault in [
        OwnerFault::Host("failed revision".into()),
        OwnerFault::Panicked("owner panic".into()),
        OwnerFault::Lost("lost owner".into()),
    ] {
        gate.publish(OwnerState::Ready {
            key: key(),
            mode: ServiceMode::Embedded,
        });
        assert!(snapshot.configuration_in(&slot).is_some());
        gate.publish(OwnerState::Failed(fault));
        assert!(
            snapshot.configuration_in(&slot).is_none(),
            "host may still be held while waiting for Retry"
        );
        assert!(gate.restart());
        assert!(
            snapshot.configuration_in(&slot).is_none(),
            "Retry is Starting, not a live owner"
        );
    }
    gate.publish(OwnerState::Ready {
        key: key(),
        mode: ServiceMode::Embedded,
    });
    gate.close();
    gate.publish(OwnerState::Ready {
        key: key(),
        mode: ServiceMode::Embedded,
    });
    assert!(
        snapshot.configuration_in(&slot).is_none(),
        "late Ready after close cannot restore export"
    );
}

#[test]
fn attached_and_unowned_compositions_do_not_guess_compilers_or_policy() {
    let slot = RwLock::new(None);
    let lifetime = Arc::new(());
    let gate = OwnerGate::ready(key(), ServiceMode::Attached);
    let lease = lease_into(&slot, composition(None));
    assert!(current(&slot).configuration_in(&slot).is_none());
    drop(lease);
    let _lease = lease_into(
        &slot,
        composition(Some(context(&lifetime, &gate, "/old/rustc"))),
    );
    assert!(
        current(&slot).configuration_in(&slot).is_none(),
        "attached readiness cannot authorize an embedded owner's saved context"
    );
}

struct FixtureVerifier;
impl ProducerObservationVerifier for FixtureVerifier {
    type Error = &'static str;
    fn verify(
        &self,
        observation: &UntrustedProducerObservation,
    ) -> Result<ProducerObservationClaims, Self::Error> {
        if observation.evidence() != b"configuration-fixture" {
            return Err("wrong fixture evidence");
        }
        Ok(ProducerObservationClaims::new(
            observation.producer_identity(),
            observation.scope_root(),
            observation.context(),
            *blake3::hash(observation.evidence()).as_bytes(),
        ))
    }
}

fn root() -> Arc<ViewRoot> {
    let basis = Basis::new(
        view_state_root(&[]),
        object_version(b"configuration-source"),
    );
    let observation = admit_producer_observation(
        UntrustedProducerObservation::new(
            [7; 32],
            ScopeRoot::from_bytes(basis.object.to_bytes()),
            [9; 32],
            b"configuration-fixture".to_vec(),
        ),
        &FixtureVerifier,
    )
    .expect("fixture producer proof");
    let coverage = CoverageCapability::from_authorized_with_evidence(
        admit_complete_scope(
            AuthorityScopeClaim::from_object_version(basis.object),
            observation,
        )
        .expect("complete scope"),
        b"configuration-fixture".to_vec(),
    )
    .expect("complete coverage");
    Arc::new(
        ViewRoot::empty_checked(
            view_key(b"configuration-view"),
            basis,
            Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
            coverage,
        )
        .expect("complete root"),
    )
}

#[test]
fn same_host_reacquisition_requires_a_fresh_certified_root_before_export_resumes() {
    let slot = RwLock::new(None);
    let lifetime = Arc::new(());
    let root = root();
    let cursor = Cursor::for_view_root_at(&root, 0);
    let gate = OwnerGate::ready(
        VersionedRoot::from_revision(1, cursor, 0),
        ServiceMode::Embedded,
    );
    let _lease = lease_into(
        &slot,
        composition(Some(context(&lifetime, &gate, "/held/rustc"))),
    );
    let snapshot = current(&slot);
    let old = gate.ready_epoch().expect("old serving attachment");
    assert_eq!(
        gate.publish_view(old, Arc::clone(&root), cursor),
        PublicationAdmission::Admitted
    );
    assert!(snapshot.configuration_in(&slot).is_some());
    let (fresh, _) = gate.replace_observation(old).expect("replacement observer");
    assert!(snapshot.configuration_in(&slot).is_none());
    assert!(
        !gate.complete_observation(fresh),
        "a retained root is not fresh proof"
    );
    assert_eq!(
        gate.publish_view(old, Arc::clone(&root), cursor),
        PublicationAdmission::Withdrawn
    );
    assert!(
        snapshot.configuration_in(&slot).is_none(),
        "an old observer cannot reopen export"
    );
    assert_eq!(
        gate.publish_view(fresh, root, cursor),
        PublicationAdmission::Admitted
    );
    assert!(gate.complete_observation(fresh));
    let restored = snapshot
        .configuration_in(&slot)
        .expect("same host with fresh certified publication");
    assert_eq!(
        restored.compiler_environment,
        configuration("/held/rustc").compiler_environment
    );
    assert_eq!(
        restored.runtime_policy,
        configuration("/held/rustc").runtime_policy
    );
}
